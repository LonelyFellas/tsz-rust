use anyhow::Context;
use tsz_rust::lexicon::v3_initial_headword_backfill;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let command = std::env::args()
        .nth(1)
        .context("usage: lexicon_v3_initial_headwords <dry-run|apply>")?;
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pool = tsz_rust::platform::connect_db(&database_url).await?;
    let report = match command.as_str() {
        "dry-run" => serde_json::to_value(v3_initial_headword_backfill::dry_run(&pool).await?)?,
        "apply" => serde_json::to_value(
            v3_initial_headword_backfill::apply(
                &pool,
                &required_string("MIGRATION_MANIFEST_DIGEST")?,
            )
            .await?,
        )?,
        _ => anyhow::bail!("unknown command: {command}"),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn required_string(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("{name} is required"))
}
