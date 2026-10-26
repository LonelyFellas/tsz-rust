use super::{CatalogVoice, Voice};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use utoipa::ToSchema;

/// Descriptive provider facts, independent of the resource's synthesis capabilities.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct VoiceMetadata {
    pub provider_voice_id: String,
    pub model: String,
    pub age_group: Option<String>,
    pub description: Option<String>,
    pub personalities: Option<Vec<String>>,
    pub scenarios: Option<Vec<String>>,
    pub roles: Option<Vec<String>>,
    pub source_url: String,
    pub checked_at: DateTime<Utc>,
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<VoiceMetadata>,
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

pub fn voice_metadata(provider_voice_id: &str) -> Option<VoiceMetadata> {
    catalog_snapshot()
        .iter()
        .find(|entry| entry.provider_voice_id == provider_voice_id)
        .and_then(|entry| entry.metadata.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::preview::dto::{VoiceCapabilities, VoiceResponse};

    #[test]
    fn metadata_snapshot_matches_existing_voices_and_preserves_capabilities() {
        let entries = catalog_snapshot();
        assert_eq!(entries.len(), 126);
        for entry in entries {
            let metadata = voice_metadata(&entry.provider_voice_id).unwrap();
            assert_eq!(metadata.provider_voice_id, entry.provider_voice_id);
            assert!(matches!(
                metadata.model.as_str(),
                "neural" | "multilingual" | "hd_flash"
            ));
            assert!(matches!(
                metadata.age_group.as_deref(),
                None | Some("Adult" | "YoungAdult" | "Child" | "Senior")
            ));
            assert!(!metadata.description.as_deref().unwrap().is_empty());
            assert_eq!(metadata.roles, Some(vec![]));
            assert!(crate::speech::locale::EnglishLocale::parse(&entry.locale).is_some());
        }
        assert_eq!(
            voice_metadata("en-US-AnaNeural")
                .unwrap()
                .age_group
                .as_deref(),
            Some("Child")
        );
        assert_eq!(
            voice_metadata("en-US-AndrewMultilingualNeural")
                .unwrap()
                .model,
            "multilingual"
        );
        assert!(
            voice_metadata("en-GB-AlfieNeural")
                .unwrap()
                .personalities
                .is_none()
        );
        let ryan = entries
            .iter()
            .find(|entry| entry.provider_voice_id == "en-GB-RyanNeural")
            .unwrap();
        assert_eq!(ryan.styles, ["chat", "cheerful"]);
        assert!(
            voice_metadata("en-us-aria").is_none(),
            "aliases are not provider IDs"
        );
        assert!(voice_metadata("en-US-UnknownNeural").is_none());
    }

    #[test]
    fn metadata_is_optional_and_does_not_change_synthesis_version() {
        let mut legacy: CatalogEntry = serde_json::from_value(serde_json::json!({
            "provider_voice_id": "en-US-AriaNeural",
            "locale": "en-US",
            "gender": "female",
            "styles": ["chat"]
        }))
        .unwrap();
        assert!(legacy.metadata.is_none());
        let version = legacy.catalog_voice().version();
        legacy.metadata = voice_metadata(&legacy.provider_voice_id);
        assert_eq!(legacy.catalog_voice().version(), version);
        assert_eq!(
            legacy.catalog_voice().voice.styles().collect::<Vec<_>>(),
            ["chat"]
        );
    }

    #[test]
    fn voice_response_omits_missing_metadata_and_keeps_unknown_tags_null() {
        let mut response = VoiceResponse {
            alias: "operator-kept-alias".into(),
            display_name: "Alfie".into(),
            is_common: true,
            accent: None,
            accent_revision: None,
            synthesis_available: None,
            capability_evidence: None,
            locale: "en-GB".into(),
            gender: "male".into(),
            capabilities: VoiceCapabilities {
                styles: vec![],
                min_rate_percent: -10,
                max_rate_percent: 100,
                min_pitch_semitones: -12,
                max_pitch_semitones: 12,
                supports_volume_percent: None,
                supports_pause: None,
                supports_phoneme: None,
            },
            metadata: None,
        };
        let without = serde_json::to_value(&response).unwrap();
        assert!(without.get("metadata").is_none());
        response.metadata = voice_metadata("en-GB-AlfieNeural");
        let with = serde_json::to_value(response).unwrap();
        assert_eq!(with["metadata"]["provider_voice_id"], "en-GB-AlfieNeural");
        assert!(with["metadata"]["personalities"].is_null());
        assert_eq!(with["metadata"]["roles"], serde_json::json!([]));
        let mut original_fields = with.as_object().unwrap().clone();
        original_fields.remove("metadata");
        assert_eq!(serde_json::Value::Object(original_fields), without);
    }
}
