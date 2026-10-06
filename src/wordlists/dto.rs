use crate::{
    api::PaginationMeta,
    lexicon::dto::{Dialect, RichTextV3, WordEntryKindV3},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WordlistState {
    Draft,
    Pending,
    Published,
    Rejected,
    Withdrawn,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct Wordlist {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub owner_name: String,
    pub name: String,
    pub state: WordlistState,
    pub revision: i64,
    pub item_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistPage {
    pub items: Vec<Wordlist>,
    pub pagination: PaginationMeta,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct WordlistQuery {
    #[param(max_length = 100)]
    pub q: Option<String>,
    #[param(minimum = 1, default = 1)]
    pub page: Option<u32>,
    #[param(minimum = 1, maximum = 100, default = 50)]
    pub page_size: Option<u32>,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWordlistItem {
    pub entry_id: Uuid,
    #[schema(max_length = 1000)]
    pub private_note: String,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWordlist {
    pub idempotency_key: Uuid,
    #[schema(min_length = 1, max_length = 100)]
    pub name: String,
    #[schema(min_items = 1, max_items = 10000)]
    pub items: Vec<CreateWordlistItem>,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistNoteUpdate {
    pub entry_id: Uuid,
    #[schema(minimum = 1)]
    pub expected_note_revision: i64,
    #[schema(max_length = 1000)]
    pub private_note: String,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistContentUpdate {
    #[schema(min_length = 1, max_length = 100)]
    pub name: String,
    #[schema(min_items = 1, max_items = 10000)]
    pub entry_ids: Vec<Uuid>,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateWordlist {
    #[schema(minimum = 1)]
    pub expected_revision: i64,
    #[schema(required = true)]
    pub content: Option<WordlistContentUpdate>,
    #[schema(max_items = 10000)]
    pub note_updates: Vec<WordlistNoteUpdate>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistEditSnapshot {
    pub wordlist: Wordlist,
    pub entry_ids: Vec<Uuid>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistText {
    pub dialect: Dialect,
    pub content: RichTextV3,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistDefinition {
    pub id: Uuid,
    pub definition_mode: String,
    pub level: String,
    #[schema(required = true)]
    pub grammar_structure_id: Option<Uuid>,
    pub texts: Vec<WordlistText>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistSense {
    pub id: Uuid,
    pub sub_pos: String,
    pub level: String,
    pub definitions: Vec<WordlistDefinition>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistGrammar {
    pub id: Uuid,
    pub variants: Vec<WordlistText>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistPos {
    pub pos_id: Uuid,
    pub pos: String,
    pub senses: Vec<WordlistSense>,
    pub grammar_structures: Vec<WordlistGrammar>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistEntry {
    pub entry_id: Uuid,
    pub publication_id: Uuid,
    pub label: String,
    pub kind: WordEntryKindV3,
    pub pos: Vec<WordlistPos>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistItem {
    pub entry_id: Uuid,
    pub position: i32,
    /// Null means unavailable; no historical label or definition is returned.
    #[schema(required = true)]
    pub entry: Option<WordlistEntry>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MyWordlistItem {
    pub entry_id: Uuid,
    pub position: i32,
    #[schema(required = true)]
    pub entry: Option<WordlistEntry>,
    pub private_note: String,
    pub note_revision: i64,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistItems {
    pub items: Vec<WordlistItem>,
    pub revision: i64,
    pub pagination: PaginationMeta,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MyWordlistItems {
    pub items: Vec<MyWordlistItem>,
    pub revision: i64,
    pub pagination: PaginationMeta,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistCandidate {
    pub entry_id: Uuid,
    pub publication_id: Uuid,
    pub label: String,
    pub glosses: Vec<String>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistCatalog {
    pub items: Vec<WordlistCandidate>,
    pub pagination: PaginationMeta,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitWordlist {
    pub expected_revision: i64,
    pub idempotency_key: Uuid,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WithdrawWordlist {
    pub expected_revision: i64,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistDecision {
    pub expected_revision: i64,
    pub approve: bool,
    #[schema(required = true, max_length = 1000)]
    pub reason: Option<String>,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWithdrawWordlist {
    pub expected_revision: i64,
    #[schema(min_length = 1, max_length = 1000)]
    pub reason: String,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct WordlistReview {
    pub id: Uuid,
    pub wordlist_id: Uuid,
    pub submitted_revision: i64,
    pub name: String,
    pub state: String,
    #[schema(required = true)]
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    #[schema(required = true)]
    pub decided_at: Option<DateTime<Utc>>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistReviews {
    #[schema(required = true)]
    pub withdraw_reason: Option<String>,
    pub items: Vec<WordlistReview>,
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct AdminWordlistQuery {
    pub q: Option<String>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub state: Option<WordlistState>,
}
