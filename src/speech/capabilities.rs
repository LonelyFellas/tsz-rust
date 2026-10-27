use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct VoiceCapabilityEvidence {
    pub provider_voice_id: String,
    pub locale: String,
    pub gender: String,
    pub model: String,
    pub styles: Vec<String>,
    pub supported_locales: Option<Vec<String>>,
    pub roles: Option<Vec<String>>,
    pub supports_style_degree: Option<bool>,
    pub source_url: String,
    pub checked_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct CapabilitySnapshot {
    pub items: Vec<VoiceCapabilityEvidence>,
}

pub fn snapshot() -> &'static CapabilitySnapshot {
    static SNAPSHOT: OnceLock<CapabilitySnapshot> = OnceLock::new();
    SNAPSHOT.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../ops/speech-voice-catalog/capability-evidence.json"
        ))
        .expect("checked-in capability evidence is valid")
    })
}

pub fn for_voice(provider_voice_id: &str) -> Option<&'static VoiceCapabilityEvidence> {
    snapshot()
        .items
        .iter()
        .find(|item| item.provider_voice_id == provider_voice_id)
}

pub fn synthesis_available(provider_voice_id: &str, locale: &str) -> bool {
    super::locale::EnglishLocale::parse(locale).is_some()
        && for_voice(provider_voice_id).is_some_and(|item| item.locale == locale)
}
