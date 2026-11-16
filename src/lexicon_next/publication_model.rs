use super::{model::NextDraft, sentence_model::NextSentence};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextPublicationBase {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(minimum = 1)]
    pub base_lifecycle_revision: i64,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextPublishInput {
    pub request_id: Uuid,
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(minimum = 1)]
    pub base_lifecycle_revision: i64,
    pub preview_token: String,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextPublicationIssue {
    pub node_id: Uuid,
    pub field: String,
    pub message: String,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextPublicationPreview {
    pub entry_id: Uuid,
    pub revision: i64,
    pub lifecycle_revision: i64,
    pub preview_token: String,
    pub issues: Vec<NextPublicationIssue>,
    pub warnings: Vec<NextPublicationIssue>,
    pub can_publish: bool,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextPublicationSnapshot {
    pub draft: NextDraft,
    pub sentence_resources: Vec<NextSentence>,
    pub relation_targets: Vec<NextPublicationRelation>,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextPublicationRelation {
    pub entry_id: Uuid,
    pub sense_id: Uuid,
    pub publication_id: Uuid,
    pub headword: String,
    pub gloss: String,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextPublication {
    pub id: Uuid,
    pub entry_id: Uuid,
    pub revision: i64,
    pub lifecycle_revision: i64,
    pub published_by_admin_id: Uuid,
    pub published_at: DateTime<Utc>,
    pub snapshot: NextPublicationSnapshot,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextPublicationState {
    pub entry_id: Uuid,
    pub status: super::list_model::NextWorkspaceStatus,
    pub lifecycle_revision: i64,
    pub has_unpublished_changes: bool,
    #[schema(required)]
    pub current: Option<NextPublication>,
}
