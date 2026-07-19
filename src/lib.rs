pub mod auth;
pub mod config;
pub mod constant;
pub mod error;
pub mod openapi;
pub mod otp;
pub mod platform;
pub mod session;
pub mod state;
pub mod user;

use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::Duration;
use config::Config;
use deadpool_redis::Pool as RedisPool;
use serde_json::json;
use sqlx::PgPool;

use crate::{
    auth::{Realm, TokenManager},
    otp::{sender::OtpSender, service::OtpService, store::OtpStore},
    state::AppState,
};

/// 构建路由。** 纯函数，不绑定端口 ** -- 这是可测的接缝
/// 集成测试能拿它做oneshot, 不必真起服务器
pub fn router(state: AppState) -> Router {
    let router = Router::new()
        .route("/healthz", get(liveness))
        .route("/readyz", get(readiness))
        .nest(
            user::USER_MOUNT,
            Router::new().route("/register", post(user::handler::register)),
        )
        .nest(
            auth::AUTH_MOUNT,
            Router::new()
                .route("/login", post(auth::handler::login))
                .route("/refresh", post(auth::handler::refresh_token))
                .route("/logout", post(auth::handler::logout))
                .route("/login-otp", post(auth::handler::login_otp))
                .route("/me", get(auth::handler::me)),
        )
        .nest(
            otp::OTP_MOUNT,
            Router::new().route("/send", post(otp::handler::send_otp)),
        );

    // Swagger UI 仅在 `swagger` feature 开启时挂载：/swagger-ui 页面，spec 在 /api-docs/openapi.json。
    // release 默认不带此 feature —— 不暴露接口清单、二进制也不含 UI 资源。
    #[cfg(feature = "swagger")]
    let router = {
        use utoipa::OpenApi;
        use utoipa_swagger_ui::SwaggerUi;
        router.merge(
            SwaggerUi::new("/swagger-ui")
                .url("/api-docs/openapi.json", crate::openapi::ApiDoc::openapi()),
        )
    };

    router.with_state(state)
}

// 存活：纯进程检查，不碰DB
async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

// 就绪：DB连通性检查
async fn readiness(
    State(pool): State<PgPool>,
    State(redis): State<RedisPool>,
) -> impl IntoResponse {
    // 1) 探 DB
    if let Err(e) = sqlx::query("SELECT 1").execute(&pool).await {
        tracing::error!("database connection failed: {}", e);
        return unavailable("database connection failed");
    }

    // 2) 探 Redis
    match redis.get().await {
        Ok(mut conn) => {
            if let Err(e) = deadpool_redis::redis::cmd("PING")
                .query_async::<()>(&mut conn)
                .await
            {
                tracing::error!("redis connection failed: {}", e);
                return unavailable("redis connection failed");
            }
        }
        Err(e) => {
            tracing::error!("redis connection failed: {}", e);
            return unavailable("redis connection failed");
        }
    }

    (StatusCode::OK, Json(json!({"status": "ready"})))
}

fn unavailable(reason: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"status": "unavailable", "reason": reason})),
    )
}

/// 总入口：读配置 -> 绑端口 -> serve。绑socket的部分不适合单测
/// 所以和 router() 分开，让逻辑（router）可测、启动（run）够薄。
pub async fn run(config: Config, pool: PgPool, redis: deadpool_redis::Pool) -> anyhow::Result<()> {
    // 启动即迁移：migrations/ 于编译期内嵌进二进制（sqlx 的 `migrate` feature），
    // 部署到空库时自动建表/升级——裸机部署无需单独跑 `sqlx migrate run`，二进制自带迁移。
    // 放在 bind 之前：迁移未完成不开始收流量，避免 readyz 过早报 ready。
    // 并发安全：sqlx 迁移持有 Postgres advisory lock，多实例同时启动只有一个真正执行。
    sqlx::migrate!("./migrations").run(&pool).await?;
    tracing::info!("database migrations applied");

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on {addr}");

    // 建两个 realm 的TokenManager, secret + access TTL 从 config 读取
    let token_manager = Arc::new(TokenManager::new(
        &config.jwt_secret,
        Realm::Web,
        Duration::minutes(config.access_ttl_minutes as i64),
    ));
    let admin_token_manager = Arc::new(TokenManager::new(
        &config.admin_jwt_secret,
        Realm::Admin,
        Duration::minutes(config.admin_access_ttl_minutes as i64),
    ));

    let otp_service = Arc::new(OtpService::new(
        OtpStore::new(redis.clone()),
        OtpSender::Mock,
        StdDuration::from_secs(config.otp_cooldown_seconds), // cooldown
        config.otp_daily_limit,                              // daily_limit
        StdDuration::from_secs(config.otp_ttl_minutes * 60), // ttl
        config.otp_max_attempts.get(),                       // NonZeroU8 → u8，兑现了！
    ));
    let state = AppState {
        pool,
        token_manager,
        admin_token_manager,
        refresh_ttl: Duration::days(config.refresh_ttl_days as i64),
        admin_refresh_ttl: Duration::days(config.admin_refresh_ttl_days as i64),
        redis,
        otp_service,
        cookie_secure: config.cookie_secure,
    };

    // 接优雅停机：systemctl stop / 容器停止发 SIGTERM，Ctrl+C 发 SIGINT。
    // 收到信号后停止收新连接、放在途请求跑完再退出，避免请求被硬砍。
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// 等待停机信号：SIGTERM（systemd/容器）或 SIGINT（Ctrl+C），任一到达即返回。
async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received, starting graceful shutdown");
}
