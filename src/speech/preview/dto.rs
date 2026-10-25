use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::lexicon::dto::RichTextV2;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreatePreviewRequest {
    pub content: RichTextV2,
    pub voice_alias: String,
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
    pub cache_status: PreviewCacheStatus,
    pub audio_url: String,
    pub expires_at: DateTime<Utc>,
    pub url_expires_in_seconds: u64,
}
