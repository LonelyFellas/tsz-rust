pub mod config;

use axum::{Router, routing::get};
use config::Config;

/// 构建路由。** 纯函数，不绑定端口 ** -- 这是可测的接缝
/// 集成测试能拿它做oneshot, 不必真起服务器
pub fn router() -> Router {
    Router::new().route("/healthz", get(liveness))
}

async fn liveness() -> &'static str {
    "OK"
}

/// 总入口：读配置 -> 绑端口 -> serve。绑socket的部分不适合单测
/// 所以和 router() 分开，让逻辑（router）可测、启动（run）够薄。
pub async fn run(config: &Config) -> anyhow::Result<()> {
    serve(router(), config).await
}

async fn serve(router: Router, config: &Config) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{}", config.port); // 
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on {addr}");
    axum::serve(listener, router).await?;
    Ok(())
}
