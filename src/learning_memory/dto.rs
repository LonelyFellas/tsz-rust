use crate::learning_tasks::dto::{LearningContext, LearningPrompt};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryOverview {
    pub available: bool,
    pub enabled: bool,
    pub answer_profile: String,
    pub due_count: i64,
    pub relearning_count: i64,
    pub later_count: i64,
    pub paused_count: i64,
    pub unavailable_count: i64,
    pub server_time: DateTime<Utc>,
    pub next_day_at: DateTime<Utc>,
    #[schema(required = true)]
    pub current_session_id: Option<Uuid>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryEnrollmentInput {
    pub enabled: bool,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct MemoryQuery {
    pub filter: Option<String>,
    pub after: Option<Uuid>,
    pub limit: Option<i64>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryItem {
    pub id: Uuid,
    #[schema(required = true)]
    pub label: Option<String>,
    pub stage: String,
    pub due_at: DateTime<Utc>,
    pub paused: bool,
    pub content_available: bool,
    pub revision: i64,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryPage {
    pub items: Vec<MemoryItem>,
    #[schema(required = true)]
    pub next_cursor: Option<Uuid>,
    pub server_time: DateTime<Utc>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryPauseInput {
    pub paused: bool,
    pub expected_revision: i64,
}
#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMode {
    Due,
    Retest,
}
impl ReviewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Due => "due",
            Self::Retest => "retest",
        }
    }
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartReview {
    pub idempotency_key: Uuid,
    pub mode: ReviewMode,
    #[schema(required = true)]
    pub limit: Option<i64>,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewAnswerInput {
    pub idempotency_key: Uuid,
    pub item_id: Uuid,
    pub answer: String,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewFeedback {
    pub answer_id: Uuid,
    pub is_correct: bool,
    pub submitted_answer: String,
    pub accepted_answers: Vec<String>,
    pub memory_effect: String,
    pub accepted_at: DateTime<Utc>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewItem {
    pub id: Uuid,
    pub memory_id: Uuid,
    pub kind: String,
    pub answered: bool,
    pub content_available: bool,
    pub revealed: bool,
    #[schema(required = true)]
    pub prompt: Option<LearningPrompt>,
    #[schema(required = true)]
    pub revealed_answers: Option<Vec<String>>,
    #[schema(required = true)]
    pub feedback: Option<ReviewFeedback>,
    pub expires_at: DateTime<Utc>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewSession {
    pub id: Uuid,
    pub state: String,
    pub settings: LearningContext,
    pub items: Vec<ReviewItem>,
    pub target_count: i64,
    pub answered_count: i64,
    pub server_time: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewReceipt {
    pub answer_id: Uuid,
    pub item_id: Uuid,
    pub is_correct: bool,
    pub memory_effect: String,
    pub next_due_at: DateTime<Utc>,
    pub accepted_at: DateTime<Utc>,
    pub session: ReviewSession,
}
