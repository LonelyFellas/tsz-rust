use anyhow::Context;
use serde::Serialize;
use tsz_rust::lexicon::{
    surface_backfill::{
        SURFACE_WRITER_VERSION, execute_surface_cutover, run_surface_backfill,
        run_surface_cutover_preflight, run_surface_parity,
    },
    surface_policy::SurfacePolicyStore,
};

#[derive(Serialize)]
struct ParityEnvelope<T> {
    schema_version: u8,
    mode: &'static str,
    writer_version: &'static str,
    parity: T,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let command = std::env::args()
        .nth(1)
        .context("usage: lexicon_surface_migration <migrate|backfill|parity|preflight|cutover|policy-enable|policy-disable>")?;
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pool = tsz_rust::platform::connect_db(&database_url).await?;

    let report = match command.as_str() {
        "migrate" => {
            sqlx::migrate!("./migrations").run(&pool).await?;
            serde_json::json!({
                "schema_version": 1,
                "mode": "migrate_expand",
                "writer_version": SURFACE_WRITER_VERSION,
                "applied": true
            })
        }
        "backfill" => serde_json::to_value(run_surface_backfill(&pool).await?)?,
        "parity" => serde_json::to_value(ParityEnvelope {
            schema_version: 1,
            mode: "parity",
            writer_version: SURFACE_WRITER_VERSION,
            parity: run_surface_parity(&pool).await?,
        })?,
        "preflight" => {
            let expected = expected_writer_version()?;
            let policies = policy_store().await?;
            serde_json::to_value(run_surface_cutover_preflight(&pool, &policies, &expected).await?)?
        }
        "cutover" => {
            let expected = expected_writer_version()?;
            let policies = policy_store().await?;
            let confirmed_hash = std::env::var("CONFIRMED_CUTOVER_ARTIFACT_SHA256")
                .context("CONFIRMED_CUTOVER_ARTIFACT_SHA256 is required")?;
            serde_json::to_value(
                execute_surface_cutover(&pool, &policies, &expected, &confirmed_hash).await?,
            )?
        }
        "policy-enable" | "policy-disable" => {
            let policy = policy_store()
                .await?
                .transition_exact_headword_creation(&pool, command == "policy-enable")
                .await?;
            serde_json::to_value(policy)?
        }
        _ => anyhow::bail!("unknown command: {command}"),
    };

    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

async fn policy_store() -> anyhow::Result<SurfacePolicyStore> {
    let redis_url = std::env::var("REDIS_URL").context("REDIS_URL is required")?;
    let redis = tsz_rust::platform::connect_redis(&redis_url).await?;
    Ok(SurfacePolicyStore::new(redis))
}

fn expected_writer_version() -> anyhow::Result<String> {
    std::env::var("EXPECTED_SURFACE_WRITER_VERSION")
        .context("EXPECTED_SURFACE_WRITER_VERSION is required")
}
