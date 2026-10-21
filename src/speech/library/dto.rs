use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DubbingRecommendation {
    pub id: Uuid,
    pub name: String,
    pub voice_alias: String,
    pub parameters: VoiceParameters,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DubbingGroup {
    pub id: Uuid,
    pub name: String,
    pub areas: Vec<String>,
    pub uk: Vec<DubbingRecommendation>,
    pub us: Vec<DubbingRecommendation>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DubbingGroupsResponse {
    pub groups: Vec<DubbingGroup>,
    pub revision: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveDubbingGroupsRequest {
    pub groups: Vec<DubbingGroup>,
    pub base_revision: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VoiceParameters {
    pub rate_percent: i16,
    pub pitch_semitones: i8,
    pub style: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VoiceLibraryItem {
    pub voice_alias: String,
    pub description_zh: Option<String>,
    pub description_revision: i64,
    pub parameters: Option<VoiceParameters>,
    pub notes: String,
    pub personal_revision: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VoiceLibraryResponse {
    pub items: Vec<VoiceLibraryItem>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveVoiceSettingsRequest {
    pub parameters: Option<VoiceParameters>,
    pub notes: String,
    pub base_revision: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveVoiceDescriptionRequest {
    pub description_zh: String,
    pub base_revision: i64,
}
