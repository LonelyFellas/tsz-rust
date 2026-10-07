use crate::api::PaginationMeta;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum LearningTaskType {
    Daily,
    Longterm,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum LearningTaskState {
    Active,
    Archived,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum LearningRunState {
    Active,
    Completed,
    Expired,
    Invalidated,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningContext {
    pub cefr_level: String,
    pub english_variant: String,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewLearningTask {
    pub task_type: LearningTaskType,
    pub wordlist_ids: Vec<Uuid>,
    #[schema(required = true)]
    pub daily_question_count: Option<i32>,
    #[schema(required = true)]
    pub ends_at: Option<DateTime<Utc>>,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateLearningTask {
    pub idempotency_key: Uuid,
    pub name: String,
    pub task_type: LearningTaskType,
    pub wordlist_ids: Vec<Uuid>,
    #[schema(required = true)]
    pub daily_question_count: Option<i32>,
    #[schema(required = true)]
    pub ends_at: Option<DateTime<Utc>>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameLearningTask {
    pub expected_revision: i64,
    pub name: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchiveLearningTask {
    pub expected_revision: i64,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartLearningRun {
    pub idempotency_key: Uuid,
    pub expected_revision: i64,
    #[schema(required = true)]
    pub after_run_id: Option<Uuid>,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitLearningAnswer {
    pub idempotency_key: Uuid,
    pub question_id: Uuid,
    pub answer: String,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct LearningPageQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub state: Option<LearningTaskState>,
}
#[derive(Debug, Default, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningExclusions {
    pub unavailable_entries: i64,
    pub context_dependent: i64,
    pub no_chinese_definition: i64,
    pub no_base_form: i64,
    pub answer_in_prompt: i64,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningPreview {
    pub eligible_count: i64,
    pub entry_count: i64,
    pub exclusions: LearningExclusions,
    pub settings: LearningContext,
    pub can_start: bool,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct LearningTask {
    pub id: Uuid,
    pub name: String,
    pub task_type: LearningTaskType,
    pub wordlist_ids: Vec<Uuid>,
    #[schema(required = true)]
    pub daily_question_count: Option<i32>,
    #[schema(required = true)]
    pub ends_at: Option<DateTime<Utc>>,
    pub state: LearningTaskState,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningTaskDetail {
    pub task: LearningTask,
    pub server_time: DateTime<Utc>,
    pub business_day: NaiveDate,
    pub timezone: String,
    #[schema(required = true)]
    pub current_run: Option<LearningRun>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningTaskPage {
    pub items: Vec<LearningTaskDetail>,
    pub pagination: PaginationMeta,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningRun {
    pub id: Uuid,
    pub task_id: Uuid,
    pub task_type: LearningTaskType,
    pub state: LearningRunState,
    pub question_type: String,
    pub task_revision: i64,
    #[schema(required = true)]
    pub business_day: Option<NaiveDate>,
    #[schema(required = true)]
    pub expires_at: Option<DateTime<Utc>>,
    pub started_at: DateTime<Utc>,
    pub server_time: DateTime<Utc>,
    pub settings: LearningContext,
    pub target_count: i32,
    pub answered_count: i64,
    pub correct_count: i64,
    pub generation_version: String,
    pub grading_version: String,
    #[schema(required = true)]
    pub completion_id: Option<Uuid>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningPrompt {
    pub definition: String,
    pub part_of_speech: String,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningAnswerFeedback {
    pub answer_id: Uuid,
    pub submitted_answer: String,
    pub is_correct: bool,
    pub accepted_at: DateTime<Utc>,
    pub accepted_answers: Vec<String>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningQuestion {
    pub id: Uuid,
    pub position: i32,
    pub answered: bool,
    pub content_available: bool,
    #[schema(required = true)]
    pub prompt: Option<LearningPrompt>,
    #[schema(required = true)]
    pub feedback: Option<LearningAnswerFeedback>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningQuestionPage {
    pub items: Vec<LearningQuestion>,
    pub pagination: PaginationMeta,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningRunPage {
    pub items: Vec<LearningRun>,
    pub pagination: PaginationMeta,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningAnswerReceipt {
    pub answer_id: Uuid,
    pub question_id: Uuid,
    pub is_correct: bool,
    pub accepted_at: DateTime<Utc>,
    pub run: LearningRun,
    pub question: LearningQuestion,
}
