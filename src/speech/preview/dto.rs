use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::lexicon::dto::RichTextV2;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreatePreviewRequest {
    #[serde(default)]
    pub output_format: Option<crate::speech::AudioOutputFormat>,
    pub content: RichTextV2,
    pub voice_alias: String,
    #[serde(default)]
    #[schema(minimum = 0.01, maximum = 2)]
    pub style_degree: Option<f64>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub rate_percent: i16,
    #[serde(default)]
    pub pitch_semitones: i8,
    /// Absolute synthesis volume; omitted values retain the original volume (100).
    #[serde(
        default,
        deserialize_with = "crate::speech::model::deserialize_optional_volume"
    )]
    #[schema(minimum = 0, maximum = 100, nullable = false)]
    pub volume_percent: Option<i16>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualPreviewRequest {
    pub voice_alias: String,
    #[schema(max_length = 32000)]
    pub ssml: String,
    #[serde(default)]
    pub output_format: crate::speech::AudioOutputFormat,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderSsmlRequest {
    pub source: crate::speech::generated_source::GeneratedSpeechSource,
    pub setting: crate::lexicon::dto::VoiceSettingV3,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RenderSsmlResponse {
    pub ssml: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VoiceCapabilities {
    pub styles: Vec<String>,
    pub min_rate_percent: i16,
    pub max_rate_percent: i16,
    pub min_pitch_semitones: i16,
    pub max_pitch_semitones: i16,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub supports_volume_percent: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub supports_pause: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub supports_phoneme: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VoiceResponse {
    pub alias: String,
    pub display_name: String,
    pub is_common: bool,
    pub locale: String,
    pub gender: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<crate::speech::locale::VoiceAccent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent_revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis_available: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_evidence: Option<crate::speech::capabilities::VoiceCapabilityEvidence>,
    pub capabilities: VoiceCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<crate::speech::VoiceMetadata>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VoiceListResponse {
    pub items: Vec<VoiceResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreviewCacheStatus {
    Hit,
    Generated,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PreviewResponse {
    pub content_path: String,
    pub cache_status: PreviewCacheStatus,
    pub audio_url: String,
    pub expires_at: DateTime<Utc>,
    pub url_expires_in_seconds: u64,
}
