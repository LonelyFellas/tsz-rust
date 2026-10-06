use super::model::{OwnerType, WalletStatus};
use crate::api::PaginationMeta;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManualCreditCategory {
    Purchase,
    Reward,
}
impl ManualCreditCategory {
    pub fn source_type(self) -> &'static str {
        match self {
            Self::Purchase => "manual_purchase",
            Self::Reward => "manual_reward",
        }
    }
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualCreditRequest {
    pub owner_type: OwnerType,
    pub owner_id: Uuid,
    #[schema(pattern = "^[1-9][0-9]{0,18}$")]
    pub amount: String,
    pub category: ManualCreditCategory,
    /// Stable verification number for purchases, stable business event ID for rewards.
    #[schema(min_length = 1, max_length = 200)]
    pub event_id: String,
    #[schema(min_length = 1, max_length = 1000)]
    pub reason: String,
    #[schema(max_length = 500)]
    pub evidence_ref: Option<String>,
    pub idempotency_key: Uuid,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualReversalRequest {
    pub idempotency_key: Uuid,
    #[schema(min_length = 1, max_length = 1000)]
    pub reason: String,
}
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct CoinAccount {
    pub owner_type: OwnerType,
    pub owner_id: Uuid,
    pub display_name: String,
    pub account_status: String,
    #[schema(pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub balance: String,
    pub status: WalletStatus,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CoinAccountPage {
    pub items: Vec<CoinAccount>,
    pub pagination: PaginationMeta,
}
#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct CoinAccountsQuery {
    pub owner_type: OwnerType,
    /// Name fragment, exact account UUID, phone or email. Contact details are not returned.
    pub search: String,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct ManualCoinOperation {
    pub id: Uuid,
    pub owner_type: OwnerType,
    pub owner_id: Uuid,
    pub actor_id: Uuid,
    pub source_type: String,
    pub event_id: String,
    #[schema(pattern = "^-?[1-9][0-9]{0,18}$")]
    pub delta: String,
    #[schema(pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub balance_after: String,
    pub reason: String,
    pub evidence_ref: Option<String>,
    pub reverses_operation_id: Option<Uuid>,
    pub reversed_by_operation_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualCoinOperationPage {
    pub items: Vec<ManualCoinOperation>,
    pub pagination: PaginationMeta,
}
#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct CoinOperationsQuery {
    pub owner_type: Option<OwnerType>,
    pub owner_id: Option<Uuid>,
    pub actor_id: Option<Uuid>,
    /// manual_purchase, manual_reward or manual_reversal.
    pub source_type: Option<String>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}
