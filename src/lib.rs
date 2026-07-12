pub mod auth;
pub mod config;
pub mod error;
pub mod otp;
pub mod platform;
pub mod session;
pub mod state;
pub mod user;

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
    state::AppState,
};

/// 构建路由。** 纯函数，不绑定端口 ** -- 这是可测的接缝
/// 集成测试能拿它做oneshot, 不必真起服务器
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(liveness))
        .route("/readyz", get(readiness))
        .route("/user/register", post(user::handler::register))
        .route("/auth/login", post(auth::handler::login))
        .route("/auth/refresh", post(auth::handler::refresh_token))
        .route("/auth/logout", post(auth::handler::logout))
        .route("/otp/send", post(otp::handler::send_otp))
        .with_state(state)
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
    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on {addr}");

    // 建web realm 的TokenManager, secret + access TTL 从 config 读取
    let token_manager = std::sync::Arc::new(TokenManager::new(
        &config.jwt_secret,
        Realm::Web,
        Duration::minutes(config.access_ttl_minutes as i64),
    ));
    let state = AppState {
        pool,
        token_manager,
        refresh_ttl: Duration::days(config.refresh_ttl_days as i64),
        redis,
    };

    axum::serve(listener, router(state)).await?;
    Ok(())
}
