use anyhow::Context;
use tsz_rust::lexicon::rich_text_repair_scan;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tsz_rust::safe_log::init();
    tsz_rust::safe_log::exit_on_error("lexicon_rich_text_repair", run().await)
}

async fn run() -> anyhow::Result<()> {
    let command = std::env::args()
        .nth(1)
        .context("usage: lexicon_rich_text_repair <dry-run|apply>")?;
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pool = tsz_rust::platform::connect_db(&database_url).await?;
    let report = match command.as_str() {
        "dry-run" => serde_json::to_value(rich_text_repair_scan::dry_run(&pool).await?)?,
        "apply" => serde_json::to_value(
            rich_text_repair_scan::apply(
                &pool,
                &std::env::var("MIGRATION_MANIFEST_DIGEST")
                    .context("MIGRATION_MANIFEST_DIGEST is required")?,
            )
            .await?,
        )?,
        _ => anyhow::bail!("unknown command: {command}"),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
