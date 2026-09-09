//! Explicit maintenance only: Azure calls never run on the directory request path.
use anyhow::{Context, ensure};
use tsz_rust::speech::{
    AzureSpeechConfig, SpeechProvider, preview::PreviewRepository, product_voices,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let config = AzureSpeechConfig::from_pairs(std::env::vars())?
        .context("Azure Speech must be configured")?;
    let remote = config
        .build_provider()?
        .list_voices()
        .await?
        .context("provider does not expose a catalog")?;
    ensure!(
        product_voices().iter().all(|common| remote
            .iter()
            .any(|voice| voice.voice.provider_voice_id() == common.provider_voice_id)),
        "Azure did not return every configured common voice; database left unchanged"
    );
    let pool = tsz_rust::platform::connect_db(&std::env::var("DATABASE_URL")?).await?;
    PreviewRepository::new(pool).sync_voices(&remote).await?;
    println!("Updated {} available voices", remote.len());
    Ok(())
}
