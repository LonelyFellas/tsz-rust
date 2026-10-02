use anyhow::{Context, ensure};
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    ensure!(
        args.next().as_deref() == Some("--database-url"),
        "usage: permission_migration_preview --database-url <explicit database URL>"
    );
    let url = args.next().context("explicit database URL required")?;
    ensure!(
        args.next().is_none(),
        "unexpected argument; this command only previews and never applies changes"
    );
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .context("connect for read-only migration preview")?;
    let preview = tsz_rust::admin::permissions::migration::preview(&pool).await?;
    println!("{}", serde_json::to_string_pretty(&preview)?);
    Ok(())
}
