use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
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
pub struct QuestionEntryRead {
    pub candidate_ref: String,
    pub source: QuestionSource,
    pub answer_profile: String,
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
pub struct LearningCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub choice_policies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub practice_modes: Option<Vec<String>>,
    pub question_types: Vec<String>,
}
