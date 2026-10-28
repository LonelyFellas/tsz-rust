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
