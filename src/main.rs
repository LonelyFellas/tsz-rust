use tsz_rust::config;
use tsz_rust::platform;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化追踪器
    tracing_subscriber::fmt::init();

    let config = config::load_config()?;
    let pool = platform::connect(&config.database_url).await?;
    tsz_rust::run(config, pool).await
}
