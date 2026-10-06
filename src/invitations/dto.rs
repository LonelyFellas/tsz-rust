use crate::api::PaginationMeta;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InvitationOverview {
    #[schema(pattern = "^[A-F0-9]{16}$", required = true)]
    pub invite_code: Option<String>,
    /// Effective configuration; null means rewards are disabled.
    #[schema(pattern = "^[1-9][0-9]{0,18}$", required = true)]
    pub reward_amount: Option<String>,
    pub can_receive_reward: bool,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InvitationCode {
    #[schema(pattern = "^[A-F0-9]{16}$")]
    pub invite_code: String,
}
#[derive(Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum InvitationRewardStatus {
    Awarded,
    RewardDisabled,
    InviterUnavailable,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct InvitationRecord {
    pub invitee_user_id: Uuid,
    #[schema(required = true)]
    pub invitee_name: Option<String>,
    pub reward_status: InvitationRewardStatus,
    #[schema(pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub reward_amount: String,
    pub created_at: DateTime<Utc>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InvitationRecordPage {
    pub items: Vec<InvitationRecord>,
    pub pagination: PaginationMeta,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct InvitationRecordsQuery {
    #[param(default = 1, minimum = 1)]
    pub page: Option<u32>,
    #[param(default = 20, minimum = 1, maximum = 100)]
    pub page_size: Option<u32>,
}
