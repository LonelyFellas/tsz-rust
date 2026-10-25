use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::Voice;

/// Provider metadata only; persistence keeps the existing product alias and operator settings.
#[derive(Debug, Clone)]
pub struct CatalogVoice {
    pub voice: Voice,
    pub gender: String,
}

impl CatalogVoice {
    pub fn alias(&self) -> String {
        let name = self.voice.provider_voice_id().to_ascii_lowercase();
        if self.voice.is_hd_flash() {
            let hash = hex_digest(name.as_bytes());
            return format!("azure-hd-flash-{}", &hash[..40]);
        }
        if name.len() <= 64 {
            return name;
        }
        let hash = hex_digest(name.as_bytes());
        format!("{}-{}", &name[..31], &hash[..32])
    }

    pub fn version(&self) -> String {
        let mut facts = serde_json::json!([
            self.voice.provider(),
            self.voice.provider_voice_id(),
            self.voice.locale(),
            self.gender,
            self.voice.styles().collect::<Vec<_>>()
        ]);
        if self.voice.is_hd_flash() {
            facts
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!(super::model::HD_FLASH_BUILDER_VERSION));
        }
        format!("azure-catalog-{}", hex_digest(facts.to_string().as_bytes()))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AzureVoice {
    short_name: String,
    locale: String,
    gender: String,
    voice_type: String,
    status: String,
    #[serde(default)]
    style_list: Vec<String>,
    #[serde(default)]
    voice_tag: Option<AzureVoiceTag>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AzureVoiceTag {
    #[serde(default)]
    model_series: Vec<String>,
}

pub(super) fn parse_catalog(bytes: &[u8]) -> Result<Vec<CatalogVoice>, serde_json::Error> {
    let voices: Vec<AzureVoice> = serde_json::from_slice(bytes)?;
    Ok(voices
        .into_iter()
        .filter_map(|item| {
            if super::locale::EnglishLocale::parse(&item.locale).is_none()
                || item.status != "GA"
                || item.voice_type != "Neural"
                || !matches!(item.gender.as_str(), "Female" | "Male" | "Neutral")
                || item.voice_tag.as_ref().is_some_and(|tag| {
                    tag.model_series.iter().any(|series| {
                        !matches!(series.as_str(), "Monolingual" | "Multilingual" | "HDFlash")
                    })
                })
            {
                return None;
            }
            let voice = Voice::new("azure", item.short_name, item.locale, item.style_list).ok()?;
            if voice.is_hd_flash()
                != item
                    .voice_tag
                    .as_ref()
                    .is_some_and(|tag| tag.model_series == ["HDFlash"])
            {
                return None;
            }
            Some(CatalogVoice {
                voice,
                gender: item.gender.to_ascii_lowercase(),
            })
        })
        .collect())
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
