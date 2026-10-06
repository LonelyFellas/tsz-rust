use crate::user::model::AccountDeletionChannel;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum DeletionStatus {
    Pending,
    Cancelled,
    Completed,
}
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct AccountDeletionRequest {
    pub id: Uuid,
    pub status: DeletionStatus,
    pub requested_at: DateTime<Utc>,
    pub effective_at: DateTime<Utc>,
    #[schema(required = true)]
    pub cancelled_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub completed_at: Option<DateTime<Utc>>,
    #[schema(pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub confirmed_balance: String,
    pub waive_balance: bool,
    pub consent_version: String,
    pub consent_text: String,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountDeletionState {
    #[schema(required = true)]
    pub request: Option<AccountDeletionRequest>,
    #[schema(pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub coin_balance: String,
    pub consent_version: String,
    pub consent_text: String,
    pub server_time: DateTime<Utc>,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAccountDeletionRequest {
    pub channel: AccountDeletionChannel,
    #[schema(min_length = 6, max_length = 6, pattern = "^[0-9]{6}$")]
    pub code: String,
    #[schema(pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub expected_coin_balance: String,
    pub waive_balance: bool,
    pub confirm_deletion: bool,
    pub consent_version: String,
    pub idempotency_key: Uuid,
}
