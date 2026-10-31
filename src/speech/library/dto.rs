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
    #[serde(default = "selected_default")]
    pub selected: bool,
}

fn selected_default() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DubbingGroup {
    pub id: Uuid,
    pub name: String,
    pub areas: Vec<String>,
    pub uk: Vec<DubbingRecommendation>,
    pub us: Vec<DubbingRecommendation>,
    #[serde(default)]
    pub revision: i64,
    #[serde(default)]
    pub is_global: bool,
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

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveDubbingGroupRequest {
    pub group: DubbingGroup,
    pub base_revision: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteDubbingGroupRequest {
    pub base_revision: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VoiceParameters {
    pub rate_percent: i16,
    pub pitch_semitones: i8,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::speech::model::deserialize_optional_volume"
    )]
    #[schema(minimum = 0, maximum = 100, nullable = false)]
    pub volume_percent: Option<i16>,
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 0.01, maximum = 2)]
    pub style_degree: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VoiceLibraryItem {
    pub voice_alias: String,
    pub accent: crate::speech::locale::VoiceAccent,
    pub accent_revision: i64,
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

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveVoiceAccentRequest {
    pub accent: crate::speech::locale::VoiceAccent,
    pub base_revision: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreVoiceDescriptionRequest {
    pub base_revision: i64,
}

/// A pair owns selection and order; each side retains its existing version identity.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DubbingPairVoice {
    pub id: Uuid,
    pub voice_alias: String,
    pub parameters: VoiceParameters,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DubbingRecommendationPair {
    pub id: Uuid,
    pub name: String,
    pub selected: bool,
    pub uk: DubbingPairVoice,
    pub us: DubbingPairVoice,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum DubbingRecommendationsV3 {
    Legacy {
        uk: Vec<DubbingRecommendation>,
        us: Vec<DubbingRecommendation>,
    },
    Paired {
        #[schema(max_items = 20)]
        pairs: Vec<DubbingRecommendationPair>,
    },
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DubbingGroupV3 {
    pub id: Uuid,
    pub name: String,
    pub areas: Vec<String>,
    #[schema(minimum = 0)]
    pub revision: i64,
    pub is_global: bool,
    pub recommendations: DubbingRecommendationsV3,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DubbingGroupsV3Response {
    pub groups: Vec<DubbingGroupV3>,
    #[schema(minimum = 0)]
    pub revision: i64,
    #[schema(minimum = 0)]
    pub order_revision: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PairedDubbingGroup {
    pub id: Uuid,
    pub name: String,
    pub areas: Vec<String>,
    pub is_global: bool,
    #[schema(max_items = 20)]
    pub pairs: Vec<DubbingRecommendationPair>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SavePairedDubbingGroupRequest {
    pub group: PairedDubbingGroup,
    #[schema(minimum = 0)]
    pub base_revision: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum DubbingGroupWriteRequest {
    Legacy(SaveDubbingGroupRequest),
    Paired(SavePairedDubbingGroupRequest),
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum DubbingGroupsReadResponse {
    Legacy(DubbingGroupsResponse),
    Paired(DubbingGroupsV3Response),
}

/// Only persisted identities participate; group content drafts are never accepted here.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveDubbingGroupOrderRequest {
    pub group_ids: Vec<Uuid>,
    #[schema(minimum = 0)]
    pub base_revision: i64,
}
