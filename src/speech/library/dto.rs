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
    #[schema(required = true)]
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
    #[schema(max_items = 20)]
    pub uk: Vec<DubbingRecommendation>,
    #[schema(max_items = 20)]
    pub us: Vec<DubbingRecommendation>,
    #[serde(default)]
    #[schema(required = true, minimum = 0)]
    pub revision: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub is_global: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DubbingGroupsResponse {
    pub groups: Vec<DubbingGroup>,
    pub revision: i64,
    pub order_revision: i64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UnversionedDubbingGroupsResponse {
    pub groups: Vec<UnversionedDubbingGroup>,
    pub revision: i64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UnversionedDubbingGroup {
    pub id: Uuid,
    pub name: String,
    pub areas: Vec<String>,
    pub uk: Vec<serde_json::Value>,
    pub us: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_global: Option<bool>,
}

/// Compatibility is read-only: independent sides are never fabricated into pairs.
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum DubbingGroupsReadResponse {
    Independent(DubbingGroupsResponse),
    Legacy(LegacyDubbingGroupsResponse),
    Unversioned(UnversionedDubbingGroupsResponse),
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LegacyDubbingGroupsResponse {
    pub groups: Vec<LegacyDubbingGroup>,
    pub revision: i64,
    pub order_revision: i64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LegacyDubbingGroup {
    pub id: Uuid,
    pub name: String,
    pub areas: Vec<String>,
    pub revision: i64,
    pub is_global: bool,
    pub recommendations: LegacySideRecommendations,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum LegacySideRecommendations {
    Legacy {
        uk: Vec<DubbingRecommendation>,
        us: Vec<DubbingRecommendation>,
    },
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

/// Only persisted identities participate; group content drafts are never accepted here.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveDubbingGroupOrderRequest {
    pub group_ids: Vec<Uuid>,
    #[schema(minimum = 0)]
    pub base_revision: i64,
}
