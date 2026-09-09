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
        if name.len() <= 64 {
            return name;
        }
        let hash = hex_digest(name.as_bytes());
        format!("{}-{}", &name[..31], &hash[..32])
    }

    pub fn version(&self) -> String {
        let facts = serde_json::json!([
            self.voice.provider(),
            self.voice.provider_voice_id(),
            self.voice.locale(),
            self.gender,
            self.voice.styles().collect::<Vec<_>>()
        ]);
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
}

pub(super) fn parse_catalog(bytes: &[u8]) -> Result<Vec<CatalogVoice>, serde_json::Error> {
    let voices: Vec<AzureVoice> = serde_json::from_slice(bytes)?;
    Ok(voices
        .into_iter()
        .filter_map(|item| {
            if !matches!(item.locale.as_str(), "en-GB" | "en-US")
                || item.status != "GA"
                || item.voice_type != "Neural"
                || !matches!(item.gender.as_str(), "Female" | "Male")
            {
                return None;
            }
            // HD/multi-talker identifiers that cannot be represented by the current SSML model
            // are deliberately excluded instead of advertising an unusable voice.
            let voice = Voice::new("azure", item.short_name, item.locale, item.style_list).ok()?;
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
