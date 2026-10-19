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
    #[schema(required = true)]
    pub owner_user_id: Option<Uuid>,
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
#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WordlistView {
    #[default]
    Standard,
    Full,
}
#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WordlistSort {
    #[default]
    Author,
    LabelAsc,
    LabelDesc,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct WordlistItemsQuery {
    #[param(max_length = 100)]
    pub q: Option<String>,
    #[param(minimum = 1, default = 1)]
    pub page: Option<u32>,
    #[param(minimum = 1, maximum = 100, default = 50)]
    pub page_size: Option<u32>,
    /// Full explicitly opts into form data; omitted preserves the standard wire shape.
    #[serde(default)]
    pub view: WordlistView,
    #[serde(default)]
    pub sort: WordlistSort,
}
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CustomWord {
    #[schema(min_length = 1, max_length = 50)]
    pub word: String,
    #[schema(min_length = 1, max_length = 200)]
    pub definition: String,
}
#[derive(Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistContentItem {
    pub item_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub entry_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub custom_word: Option<CustomWord>,
    #[serde(default)]
    #[schema(max_length = 1000)]
    pub author_note: String,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWordlistItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub entry_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub item_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub custom_word: Option<CustomWord>,
    #[schema(max_length = 1000)]
    pub private_note: String,
    /// Author's annotation, visible to anyone who can read the wordlist.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[schema(max_length = 1000)]
    pub author_note: String,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWordlist {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<Uuid>,
    pub idempotency_key: Uuid,
    #[schema(min_length = 1, max_length = 100)]
    pub name: String,
    #[schema(min_items = 1, max_items = 10000)]
    pub items: Vec<CreateWordlistItem>,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistNoteUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub entry_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub item_id: Option<Uuid>,
    #[schema(minimum = 1)]
    pub expected_note_revision: i64,
    #[schema(max_length = 1000)]
    pub private_note: String,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistAuthorNote {
    pub entry_id: Uuid,
    #[schema(max_length = 1000)]
    pub author_note: String,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistContentUpdate {
    #[schema(min_length = 1, max_length = 100)]
    pub name: String,
    #[schema(min_items = 1, max_items = 10000)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry_ids: Vec<Uuid>,
    /// Complete ordered content for clients supporting custom items; exclusive with entry_ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, min_items = 1, max_items = 10000)]
    pub items: Option<Vec<WordlistContentItem>>,
    /// Complete annotation list when present; omitted legacy writes preserve retained annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(max_items = 10000)]
    pub author_notes: Option<Vec<WordlistAuthorNote>>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub items: Option<Vec<WordlistContentItem>>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sub_pos_label: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub label: Option<String>,
    pub pos: String,
    pub senses: Vec<WordlistSense>,
    pub grammar_structures: Vec<WordlistGrammar>,
    /// Present only when the reader explicitly requests view=full.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub forms: Option<Vec<WordlistForm>>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistPronunciation {
    pub id: Uuid,
    pub dict_phonetic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub dict_phonetic_rich: Option<RichTextV3>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistFormVariant {
    pub id: Uuid,
    pub dialect: Dialect,
    pub spelling: String,
    pub pronunciations: Vec<WordlistPronunciation>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistForm {
    pub id: Uuid,
    pub form_type: String,
    pub label: String,
    pub sense_ids: Vec<Uuid>,
    pub variants: Vec<WordlistFormVariant>,
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
    #[schema(required = true)]
    pub entry_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub item_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub custom_word: Option<CustomWord>,
    pub position: i32,
    /// Returned only for clients requesting X-TSZ-Wordlist-Annotations: v1.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, max_length = 1000)]
    pub author_note: Option<String>,
    /// Null means unavailable; no historical label or definition is returned.
    #[schema(required = true)]
    pub entry: Option<WordlistEntry>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WordlistUnavailableReason {
    Archived,
    Unpublished,
    UnsupportedSchema,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MyWordlistItem {
    #[schema(required = true)]
    pub entry_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub item_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub custom_word: Option<CustomWord>,
    pub position: i32,
    /// Last published label for an unavailable reference, visible only to its owner
    /// requesting X-TSZ-Wordlist-Entry-Labels: v1. Never includes draft content.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub entry_label: Option<String>,
    /// Current reason for an unavailable reference; owner-only and returned only
    /// when X-TSZ-Wordlist-Entry-Reasons: v1 is requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub unavailable_reason: Option<WordlistUnavailableReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, max_length = 1000)]
    pub author_note: Option<String>,
    #[schema(required = true)]
    pub entry: Option<WordlistEntry>,
    pub private_note: String,
    pub note_revision: i64,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistAvailability {
    pub revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, minimum = 0)]
    pub custom_count: Option<i64>,
    #[schema(minimum = 0)]
    pub available_count: i64,
    #[schema(minimum = 0)]
    pub unavailable_count: i64,
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
#[derive(Serialize, ToSchema, sqlx::FromRow)]
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

#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct AdminWordlistReview {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer_name: Option<String>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordlistReviews {
    #[schema(required = true)]
    pub withdraw_reason: Option<String>,
    pub items: Vec<AdminWordlistReview>,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct AdminWordlistReviewsQuery {
    /// Opt in to reviewer identity; omitted for older strict clients.
    #[serde(default)]
    pub include_reviewer: bool,
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

#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct AdminWordlist {
    #[sqlx(flatten)]
    pub wordlist: Wordlist,
    pub review_state: String,
    pub recommended: bool,
    /// Total coins, represented as a decimal string without precision loss.
    pub tip_amount: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub has_custom_words: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, minimum = 0)]
    pub like_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, minimum = 0)]
    pub favorite_count: Option<i64>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordlistPage {
    pub items: Vec<AdminWordlist>,
    pub pagination: PaginationMeta,
}
/// Current learning impact before a wordlist is deleted.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct WordlistDeleteImpact {
    /// Active, unexpired runs that may finish under the deletion grace.
    pub active_run_count: i64,
    /// Tasks owning at least one such run.
    pub active_task_count: i64,
    /// Distinct owners of such runs.
    pub affected_user_count: i64,
    /// Active tasks that reference the list but have no run that can continue.
    pub blocked_task_count: i64,
}
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WordlistReviewState {
    None,
    Pending,
    Approved,
    Rejected,
}
impl WordlistReviewState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct WordlistManagementQuery {
    /// Opt in to management statistics; omitted for older strict clients.
    #[serde(default)]
    pub include_stats: bool,
    pub q: Option<String>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub public: Option<bool>,
    pub review_state: Option<WordlistReviewState>,
    pub recommended: Option<bool>,
    pub created_from: Option<DateTime<Utc>>,
    /// Exclusive upper bound.
    pub created_before: Option<DateTime<Utc>>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistVisibility {
    pub expected_revision: i64,
    pub public: bool,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistRecommendation {
    pub expected_revision: i64,
    pub recommended: bool,
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct WordlistSuggestionQuery {
    #[param(max_length = 200)]
    pub q: String,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistSuggestions {
    #[schema(max_items = 3)]
    pub items: Vec<WordlistCandidate>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistImportMatchRequest {
    #[schema(min_items = 1, max_items = 100)]
    pub words: Vec<String>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WordlistImportMatchStatus {
    Matched,
    Ambiguous,
    Unmatched,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistImportMatch {
    pub status: WordlistImportMatchStatus,
    #[schema(max_items = 3)]
    pub candidates: Vec<WordlistCandidate>,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordlistImportMatches {
    /// Results preserve the request order, including repeated inputs.
    #[schema(min_items = 1, max_items = 100)]
    pub items: Vec<WordlistImportMatch>,
}

#[cfg(test)]
mod reviewer_tests {
    use super::*;

    #[test]
    fn reviewer_identity_is_optional_for_legacy_admin_clients() {
        let query: AdminWordlistReviewsQuery = serde_json::from_str("{}").unwrap();
        assert!(!query.include_reviewer);
        let mut review = AdminWordlistReview {
            id: Uuid::now_v7(),
            wordlist_id: Uuid::now_v7(),
            submitted_revision: 2,
            name: "词表".into(),
            state: "approved".into(),
            reason: None,
            created_at: Utc::now(),
            decided_at: Some(Utc::now()),
            reviewer_id: None,
            reviewer_name: None,
        };
        let legacy = serde_json::to_value(&review).unwrap();
        assert!(legacy.get("reviewer_id").is_none());
        assert!(legacy.get("reviewer_name").is_none());
        let reviewer = Uuid::now_v7();
        review.reviewer_id = Some(reviewer);
        review.reviewer_name = Some("审核老师".into());
        let enriched = serde_json::to_value(&review).unwrap();
        assert_eq!(enriched["reviewer_id"], reviewer.to_string());
        assert_eq!(enriched["reviewer_name"], "审核老师");
        assert_eq!(enriched["id"], legacy["id"]);
    }
}
