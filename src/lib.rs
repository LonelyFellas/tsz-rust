pub mod auth;
pub mod config;
pub mod error;
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
        .with_state(state)
}

// 存活：纯进程检查，不碰DB
async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

// 就绪：DB连通性检查
async fn readiness(State(pool): State<PgPool>) -> impl IntoResponse {
    match sqlx::query("SELECT 1").execute(&pool).await {
        Ok(_) => (StatusCode::OK, Json(json!({"status": "ready"}))),
        Err(e) => {
            tracing::error!("database connection failed: {}", e);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"status": "unavailable", "reason": "database connection failed"})),
            )
        }
    }
}

/// 总入口：读配置 -> 绑端口 -> serve。绑socket的部分不适合单测
/// 所以和 router() 分开，让逻辑（router）可测、启动（run）够薄。
pub async fn run(config: Config, pool: PgPool) -> anyhow::Result<()> {
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
    };

    axum::serve(listener, router(state)).await?;
    Ok(())
}
