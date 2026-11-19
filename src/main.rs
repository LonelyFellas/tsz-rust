use tsz_rust::config;
use tsz_rust::deployment_migrations;
use tsz_rust::platform;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tsz_rust::safe_log::init();
    tsz_rust::safe_log::exit_on_error("main", run().await)
}

async fn run() -> anyhow::Result<()> {
    // 初始化追踪器

    let mut arguments = std::env::args().skip(1);
    if let Some(command) = arguments.next() {
        if command == "deploy-info" {
            anyhow::ensure!(arguments.next().is_none(), "too many arguments");
            println!("{}", serde_json::to_string(&deployment_migrations::info())?);
            return Ok(());
        }
        if command == "lexicon-rich-text-repair" {
            let operation = arguments
                .next()
                .ok_or_else(|| anyhow::anyhow!("repair operation is required"))?;
            anyhow::ensure!(arguments.next().is_none(), "too many arguments");
            let database_url = std::env::var("DATABASE_URL")?;
            let pool = platform::connect_db(&database_url).await?;
            let report = match operation.as_str() {
                "dry-run" => serde_json::to_value(
                    tsz_rust::lexicon::rich_text_repair_scan::dry_run(&pool).await?,
                )?,
                "apply" => serde_json::to_value(
                    tsz_rust::lexicon::rich_text_repair_scan::apply(
                        &pool,
                        &std::env::var("MIGRATION_MANIFEST_DIGEST")?,
                    )
                    .await?,
                )?,
                _ => anyhow::bail!("unknown repair operation: {operation}"),
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
            return Ok(());
        }
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
