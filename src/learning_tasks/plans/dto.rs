use super::super::dto::*;
use crate::api::PaginationMeta;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningSchedule {
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewLearningPlan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub question_type: Option<String>,
    pub task_type: LearningTaskType,
    pub wordlist_ids: Vec<Uuid>,
    #[schema(required = true)]
    pub daily_question_count: Option<i32>,
    #[schema(required = true)]
    pub schedule: Option<LearningSchedule>,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateLearningPlan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub question_type: Option<String>,
    pub idempotency_key: Uuid,
    pub name: String,
    pub task_type: LearningTaskType,
    pub wordlist_ids: Vec<Uuid>,
    #[schema(required = true)]
    pub daily_question_count: Option<i32>,
    #[schema(required = true)]
    pub schedule: Option<LearningSchedule>,
    pub preview_fingerprint: String,
}
impl CreateLearningPlan {
    pub fn preview(&self) -> PreviewLearningPlan {
        PreviewLearningPlan {
            question_type: self.question_type.clone(),
            task_type: self.task_type,
            wordlist_ids: self.wordlist_ids.clone(),
            daily_question_count: self.daily_question_count,
            schedule: self.schedule.clone(),
        }
    }
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningPlanPreview {
    pub entry_count: i64,
    pub eligible_count: i64,
    pub custom_word_count: i64,
    pub exclusions: LearningExclusions,
    pub settings: LearningContext,
    #[schema(required = true)]
    pub estimated_learning_days: Option<i64>,
    #[schema(required = true)]
    pub available_learning_days: Option<i64>,
    pub can_create: bool,
    pub reasons: Vec<String>,
    pub preview_fingerprint: String,
    pub server_time: DateTime<Utc>,
    pub business_day: NaiveDate,
    pub timezone: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CycleOperation {
    Next,
    RestartBlocked,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewLearningCycle {
    pub operation: CycleOperation,
    pub after_cycle_id: Uuid,
    pub expected_revision: i64,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateLearningCycle {
    pub operation: CycleOperation,
    pub after_cycle_id: Uuid,
    pub expected_revision: i64,
    pub preview_fingerprint: String,
    pub idempotency_key: Uuid,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningCyclePreview {
    pub preview: LearningPlanPreview,
    pub earliest_day: NaiveDate,
    pub previous_covered_count: i64,
    pub previous_total_count: i32,
    pub previous_blocked_count: i64,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningCycle {
    pub id: Uuid,
    pub cycle_number: i32,
    #[schema(value_type=LearningCycleState)]
    pub state: String,
    pub total_count: i32,
    pub covered_count: i64,
    pub remaining_count: i64,
    pub available_remaining_count: i64,
    pub blocked_count: i64,
    pub earliest_day: NaiveDate,
    pub created_at: DateTime<Utc>,
    #[schema(required = true)]
    pub completed_at: Option<DateTime<Utc>>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningPlanSource {
    pub id: Uuid,
    pub available: bool,
    #[schema(required = true)]
    pub name: Option<String>,
    #[schema(required = true)]
    pub personal_note: Option<String>,
    #[schema(required = true)]
    pub author_name: Option<String>,
    #[schema(required = true)]
    pub state: Option<String>,
    #[schema(required = true)]
    pub item_count: Option<i64>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningPlan {
    pub task: LearningTask,
    #[schema(value_type=LearningPlanMode)]
    pub mode: String,
    #[schema(required = true)]
    pub schedule: Option<LearningSchedule>,
    pub sources: Vec<LearningPlanSource>,
    #[schema(required = true)]
    pub current_cycle: Option<LearningCycle>,
    #[schema(required = true)]
    pub current_run: Option<LearningRun>,
    pub today_available_count: i64,
    #[schema(required = true)]
    pub estimated_learning_days: Option<i64>,
    #[schema(required = true)]
    pub estimate_from_day: Option<NaiveDate>,
    pub schedule_at_risk: bool,
    #[schema(value_type=LearningPlanStatus)]
    pub status: String,
    pub can_start: bool,
    pub can_start_next_cycle: bool,
    pub can_restart_blocked_cycle: bool,
    pub reasons: Vec<String>,
    pub server_time: DateTime<Utc>,
    pub business_day: NaiveDate,
    pub next_day_at: DateTime<Utc>,
    pub timezone: String,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningPlanPage {
    pub items: Vec<LearningPlan>,
    pub pagination: PaginationMeta,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningCyclePage {
    pub items: Vec<LearningCycle>,
    pub pagination: PaginationMeta,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningPlanRun {
    pub run: LearningRun,
    #[schema(required = true)]
    pub cycle_id: Option<Uuid>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LearningPlanRunPage {
    pub items: Vec<LearningPlanRun>,
    pub pagination: PaginationMeta,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LearningPlanMode {
    LegacyV1,
    CoverageV1,
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LearningPlanStatus {
    Archived,
    ScheduleEnded,
    CycleCompleted,
    NotStarted,
    TodayCompleted,
    SourceBlocked,
    Ready,
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LearningCycleState {
    Active,
    Completed,
    Cancelled,
}
