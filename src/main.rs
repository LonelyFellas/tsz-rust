use tsz_rust::config;
use tsz_rust::deployment_migrations;
use tsz_rust::platform;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化追踪器
    tracing_subscriber::fmt::init();

    let mut arguments = std::env::args().skip(1);
    if let Some(command) = arguments.next() {
        anyhow::ensure!(
            command == "deploy-undo-migrations",
            "unknown command: {command}"
        );
        let target_version = arguments
            .next()
            .ok_or_else(|| anyhow::anyhow!("target migration version is required"))?
            .parse::<i64>()?;
        let expected_version = arguments
            .next()
            .ok_or_else(|| anyhow::anyhow!("expected migration version is required"))?
            .parse::<i64>()?;
        anyhow::ensure!(arguments.next().is_none(), "too many arguments");
        let database_url = std::env::var("DATABASE_URL")?;
        let pool = platform::connect_db(&database_url).await?;
        let report = deployment_migrations::undo(&pool, target_version, expected_version).await?;
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }

    let config = config::load_config()?;
    let pool = platform::connect_db(&config.database_url).await?;
    let redis = platform::connect_redis(&config.redis_url).await?;
    // redis.get().await?.ping::<()>().await?;
    tsz_rust::run(config, pool, redis).await
}
