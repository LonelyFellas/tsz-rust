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
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VoiceCapabilities {
    pub styles: Vec<String>,
    pub min_rate_percent: i16,
    pub max_rate_percent: i16,
    pub min_pitch_semitones: i16,
    pub max_pitch_semitones: i16,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VoiceResponse {
    pub alias: String,
    pub locale: String,
    pub gender: String,
    pub capabilities: VoiceCapabilities,
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
