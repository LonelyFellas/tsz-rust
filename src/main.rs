use tsz_rust::config;
use tsz_rust::platform;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化追踪器
    tracing_subscriber::fmt::init();

    let config = config::load_config()?;
    let pool = platform::connect_db(&config.database_url).await?;
    let redis = platform::connect_redis(&config.redis_url).await?;
    // redis.get().await?.ping::<()>().await?;
    tsz_rust::run(config, pool, redis).await
}
