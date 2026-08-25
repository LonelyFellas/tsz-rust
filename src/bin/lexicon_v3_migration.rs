use anyhow::Context;
use tsz_rust::lexicon::v3_migration;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let command = std::env::args()
        .nth(1)
        .context(
            "usage: lexicon_v3_migration <inventory|dry-run|approve|apply|verify|enable-canary|rollback>",
        )?;
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pool = tsz_rust::platform::connect_db(&database_url).await?;
    let report = match command.as_str() {
        "inventory" => serde_json::to_value(v3_migration::inventory(&pool).await?)?,
        "dry-run" => serde_json::to_value(
            v3_migration::dry_run(
                &pool,
                required_uuid("MIGRATION_BATCH_ID")?,
                required_uuid("MIGRATION_ACTOR_ADMIN_ID")?,
                required_uuid("MIGRATION_REQUEST_ID")?,
                &entry_ids()?,
            )
            .await?,
        )?,
        "approve" => serde_json::to_value(
            v3_migration::approve(
                &pool,
                required_uuid("MIGRATION_BATCH_ID")?,
                required_uuid("MIGRATION_ACTOR_ADMIN_ID")?,
                required_uuid("MIGRATION_REQUEST_ID")?,
                &required_string("MIGRATION_MANIFEST_DIGEST")?,
            )
            .await?,
        )?,
        "apply" => serde_json::to_value(
            v3_migration::apply(
                &pool,
                required_uuid("MIGRATION_BATCH_ID")?,
                required_uuid("MIGRATION_ACTOR_ADMIN_ID")?,
                required_uuid("MIGRATION_REQUEST_ID")?,
                &required_string("MIGRATION_MANIFEST_DIGEST")?,
            )
            .await?,
        )?,
        "verify" => serde_json::to_value(
            v3_migration::verify(
                &pool,
                required_uuid("MIGRATION_BATCH_ID")?,
                required_uuid("MIGRATION_ACTOR_ADMIN_ID")?,
                required_uuid("MIGRATION_REQUEST_ID")?,
            )
            .await?,
        )?,
        "enable-canary" => serde_json::to_value(
            v3_migration::enable_publication_canary(
                &pool,
                required_uuid("MIGRATION_BATCH_ID")?,
                required_uuid("MIGRATION_ENTRY_ID")?,
                required_uuid("MIGRATION_ACTOR_ADMIN_ID")?,
                required_uuid("MIGRATION_REQUEST_ID")?,
            )
            .await?,
        )?,
        "rollback" => serde_json::to_value(
            v3_migration::rollback(
                &pool,
                required_uuid("MIGRATION_BATCH_ID")?,
                required_uuid("MIGRATION_ACTOR_ADMIN_ID")?,
                required_uuid("MIGRATION_REQUEST_ID")?,
            )
            .await?,
        )?,
        _ => anyhow::bail!("unknown command: {command}"),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn required_uuid(name: &str) -> anyhow::Result<Uuid> {
    let value = required_string(name)?;
    Uuid::parse_str(&value).with_context(|| format!("{name} must be a UUID"))
}

fn required_string(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("{name} is required"))
}

fn entry_ids() -> anyhow::Result<Vec<Uuid>> {
    required_string("MIGRATION_ENTRY_IDS")?
        .split(',')
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            Uuid::parse_str(value.trim())
                .with_context(|| format!("invalid MIGRATION_ENTRY_IDS value {value}"))
        })
        .collect()
}
