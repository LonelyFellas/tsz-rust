use super::{CatalogVoice, Voice};
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
pub struct ProductVoice {
    pub provider_voice_id: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct CatalogEntry {
    pub provider_voice_id: String,
    pub locale: String,
    pub gender: String,
    pub styles: Vec<String>,
}

impl CatalogEntry {
    pub fn catalog_voice(&self) -> CatalogVoice {
        CatalogVoice {
            voice: Voice::new(
                "azure",
                &self.provider_voice_id,
                &self.locale,
                self.styles.clone(),
            )
            .expect("checked-in catalog must satisfy the speech model"),
            gender: self.gender.clone(),
        }
    }
}

pub fn product_voices() -> &'static [ProductVoice] {
    static CATALOG: OnceLock<Vec<ProductVoice>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../../ops/speech-voice-catalog/product.json"))
            .expect("checked-in common voices must be valid JSON")
    })
}

pub fn catalog_snapshot() -> &'static [CatalogEntry] {
    static CATALOG: OnceLock<Vec<CatalogEntry>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../ops/speech-voice-catalog/catalog-snapshot.json"
        ))
        .expect("checked-in voice snapshot must be valid JSON")
    })
}

pub fn voice_display_name(provider_voice_id: &str, locale: &str) -> String {
    if let Some(common) = product_voices()
        .iter()
        .find(|v| v.provider_voice_id == provider_voice_id)
    {
        return common.display_name.clone();
    }
    provider_voice_id
        .strip_prefix(&format!("{locale}-"))
        .unwrap_or(provider_voice_id)
        .trim_end_matches("Neural")
        .replace("Multilingual", "（多语种）")
}
