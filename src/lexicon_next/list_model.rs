use super::model::{NextEntryKind, NextForm};
use crate::lexicon::dto::EntryNameVariantV3;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Deserialize, IntoParams, Default)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct NextWorkspaceQuery {
    /// Restrict entries to this target language; omitted means all languages.
    pub target_language: Option<String>,
    /// Restrict entries by immutable medium ownership, including empty drafts; also scope gloss.
    pub medium_locale: Option<String>,
    pub headword: Option<String>,
    pub gloss: Option<String>,
    pub kind: Option<String>,
    pub pos: Option<String>,
    pub level: Option<String>,
    pub status: Option<String>,
    pub created_by: Option<Uuid>,
    pub updated_by: Option<Uuid>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct NextWorkspaceEntry {
    pub id: Uuid,
    pub target_language: String,
    #[schema(value_type=NextEntryKind)]
    pub kind: String,
    pub headword: String,
    /// Spellings of the first base form in the first group of the first saved part of speech.
    /// Without a part of speech, this uses the original creation search text.
    /// Returned by the list only with x-tsz-next-name-variants: v1.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    #[sqlx(skip)]
    pub name_variants: Option<Vec<EntryNameVariantV3>>,
    pub owner_id: Uuid,
    pub revision: i64,
    pub lifecycle_revision: i64,
    #[schema(value_type=NextWorkspaceStatus)]
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by_name: String,
    pub updated_by_name: String,
    #[schema(required)]
    pub annotation: Option<String>,
    pub annotation_revision: i64,
    pub annotation_visible: bool,
    pub part_of_speech: Vec<String>,
    /// Returned only when x-tsz-next-levels: v1 is negotiated.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub levels: Option<Vec<String>>,
    #[schema(required)]
    pub gloss: Option<String>,
    #[sqlx(json)]
    pub forms: Vec<NextForm>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextWorkspaceList {
    pub items: Vec<NextWorkspaceEntry>,
    pub pagination: crate::api::PaginationMeta,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct NextWorkspaceStats {
    pub total: i64,
    pub today: i64,
    pub month: i64,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct NextWorkspaceCreator {
    pub id: Uuid,
    pub display_name: String,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextListTarget {
    pub entry_id: Uuid,
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(minimum = 1)]
    pub base_lifecycle_revision: i64,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NextListAction {
    Archive,
    Restore,
    Delete,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextListCommand {
    pub request_id: Uuid,
    pub action: NextListAction,
    pub items: Vec<NextListTarget>,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextListCommandResult {
    pub entry_ids: Vec<Uuid>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAnnotationInput {
    #[schema(required)]
    pub annotation: Option<String>,
    #[schema(minimum = 1)]
    pub base_annotation_revision: i64,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAnnotationResult {
    pub entry_id: Uuid,
    #[schema(required)]
    pub annotation: Option<String>,
    pub annotation_revision: i64,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NextWorkspaceStatus {
    Draft,
    Published,
    Archived,
}
