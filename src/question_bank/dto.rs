use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionSource {
    pub entry_id: Uuid,
    pub publication_id: Uuid,
    pub pos_id: Uuid,
    pub sense_id: Uuid,
    pub definition_id: Uuid,
    pub form_id: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionCandidate {
    pub candidate_ref: String,
    pub source: QuestionSource,
    pub content_fingerprint: String,
    pub spelling: String,
    pub part_of_speech: String,
    pub level: String,
    pub definition: String,
    pub other_definitions: Vec<String>,
    pub accepted_spellings: Vec<String>,
    pub answer_profile: String,
    pub form_type: String,
    pub teaching_eligible: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionCandidatePage {
    pub contract_version: i32,
    pub items: Vec<QuestionCandidate>,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct QuestionCandidateQuery {
    pub entry_id: Option<Uuid>,
    pub query: Option<String>,
    pub question_level: String,
    pub answer_profile: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentGenerationContext {
    pub contract_version: i32,
    pub external_request_id: Uuid,
    pub target: QuestionCandidate,
    pub question_level: String,
    pub answer_profile: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentCandidateSearch {
    pub contract_version: i32,
    pub target_source: QuestionSource,
    pub question_level: String,
    pub answer_profile: String,
    #[serde(default)]
    pub exclude_candidate_refs: Vec<String>,
    pub limit: i32,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionEntryRead {
    pub candidate_ref: String,
    pub source: QuestionSource,
    pub answer_profile: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentEntriesRequest {
    pub contract_version: i32,
    pub items: Vec<QuestionEntryRead>,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateQuestionGeneration {
    pub idempotency_key: Uuid,
    pub target: QuestionEntryRead,
    pub question_level: String,
    pub answer_profile: String,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionGeneration {
    pub id: Uuid,
    pub state: String,
    pub revision: i64,
    pub created_by: Uuid,
    pub question_level: String,
    pub answer_profile: String,
    pub target: QuestionCandidate,
    pub agent_run_id: Option<Uuid>,
    pub question_id: Option<Uuid>,
    pub error_code: Option<String>,
    pub reasons: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BankQuestionOption {
    pub option_id: String,
    pub candidate: QuestionCandidate,
    pub explanation: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BankQuestionContent {
    pub target: QuestionCandidate,
    pub prompt: String,
    pub question_level: String,
    pub answer_profile: String,
    pub options: Vec<BankQuestionOption>,
    pub correct_option_id: String,
    pub explanation: String,
    pub concerns: Vec<String>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BankQuestion {
    pub id: Uuid,
    pub generation_id: Uuid,
    pub revision: i64,
    pub state: String,
    pub content: BankQuestionContent,
    pub original_content: BankQuestionContent,
    pub created_by: Uuid,
    pub reviewed_by: Option<Uuid>,
    pub review_comment: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EditBankQuestion {
    pub expected_revision: i64,
    pub prompt: String,
    pub options: Vec<QuestionEntryRead>,
    pub option_explanations: Vec<String>,
    pub explanation: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewBankQuestion {
    pub expected_revision: i64,
    pub decision: String,
    pub reason_codes: Vec<String>,
    pub comment: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelQuestionGeneration {
    pub expected_revision: i64,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct QuestionBankQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub state: Option<String>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BankQuestionPage {
    pub items: Vec<BankQuestion>,
    pub pagination: crate::api::PaginationMeta,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionGenerationPage {
    pub items: Vec<QuestionGeneration>,
    pub pagination: crate::api::PaginationMeta,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningCapabilities {
    pub question_types: Vec<String>,
}

// Separate responses keep legacy clients with strict validators compatible.
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct QuestionCandidatePageQuery {
    pub entry_id: Option<Uuid>,
    pub query: Option<String>,
    pub question_level: String,
    pub answer_profile: String,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionCandidateSearchPage {
    pub items: Vec<QuestionCandidate>,
    pub pagination: crate::api::PaginationMeta,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionBatchSelection {
    pub question_levels: Vec<String>,
    pub answer_profiles: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionBatchCoverage {
    pub question_level: String,
    pub answer_profile: String,
    pub eligible: i64,
    pub covered: i64,
    pub generating: i64,
    pub missing: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionBatchPreview {
    pub selection: QuestionBatchSelection,
    pub coverage: Vec<QuestionBatchCoverage>,
    pub missing: i64,
    pub preview_token: String,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateQuestionBatch {
    pub idempotency_key: Uuid,
    pub selection: QuestionBatchSelection,
    pub preview_token: String,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionBatch {
    pub id: Uuid,
    pub created_by: Uuid,
    pub preview: QuestionBatchPreview,
    pub state: String,
    pub total: i64,
    pub queued: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub insufficient_candidates: i64,
    pub cancelled: i64,
    pub created_at: DateTime<Utc>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionBatchPage {
    pub items: Vec<QuestionBatch>,
    pub pagination: crate::api::PaginationMeta,
}

// Workbench reads are separate from legacy responses, whose consumers validate strictly.
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct QuestionWorkbenchQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub state: Option<String>,
    pub query: Option<String>,
    pub question_level: Option<String>,
    pub answer_profile: Option<String>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionWorkbenchOption {
    pub option_id: String,
    pub spelling: String,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionWorkbenchItem {
    pub id: Uuid,
    pub revision: i64,
    pub state: String,
    pub created_by: Uuid,
    pub spelling: String,
    pub part_of_speech: String,
    pub definition: String,
    pub prompt: String,
    pub question_level: String,
    pub answer_profile: String,
    pub options: Vec<QuestionWorkbenchOption>,
    pub correct_option_id: String,
    pub source_current: bool,
    pub has_concerns: bool,
    pub updated_at: DateTime<Utc>,
}
#[derive(Default, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionStateCounts {
    pub draft: i64,
    pub approved: i64,
    pub rejected: i64,
    pub disabled: i64,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionWorkbenchPage {
    pub items: Vec<QuestionWorkbenchItem>,
    pub pagination: crate::api::PaginationMeta,
    pub counts: QuestionStateCounts,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionSourceCheck {
    pub option_id: String,
    pub current: bool,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionInspection {
    pub question: BankQuestion,
    pub sources: Vec<QuestionSourceCheck>,
    pub target_current: bool,
    pub semantic_review: Option<QuestionSemanticReview>,
    pub semantic_review_current: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionSemanticReview {
    pub verdict: QuestionSemanticVerdict,
    pub issues: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionSemanticVerdict {
    NoObviousIssue,
    NeedsRevision,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchQuestionCandidates {
    pub entry_id: Option<Uuid>,
    pub query: Option<String>,
    pub question_level: String,
    pub answer_profile: String,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub part_of_speech: Option<String>,
    #[serde(default)]
    pub exclude_entry_ids: Vec<Uuid>,
    #[serde(default)]
    pub exclude_spellings: Vec<String>,
    pub excluded_definition: Option<String>,
    pub target_spelling: Option<String>,
    #[serde(default)]
    pub editable_only: bool,
}
