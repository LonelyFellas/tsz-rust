pub mod auth;
pub mod config;
pub mod error;
pub mod platform;
pub mod session;
pub mod user;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use config::Config;
use serde_json::json;
use sqlx::PgPool;
use user::handler;

/// 构建路由。** 纯函数，不绑定端口 ** -- 这是可测的接缝
/// 集成测试能拿它做oneshot, 不必真起服务器
pub fn router(pool: PgPool) -> Router {
    Router::new()
        .route("/healthz", get(liveness))
        .route("/readyz", get(readiness))
        .route("/user/register", post(handler::register))
        .with_state(pool)
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
    axum::serve(listener, router(pool)).await?;
    Ok(())
}
