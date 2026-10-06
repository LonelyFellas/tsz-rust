use super::model::{OperationKind, OwnerType, WalletStatus};
use crate::api::PaginationMeta;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CoinWallet {
    pub owner_type: OwnerType,
    pub owner_id: Uuid,
    #[schema(pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub balance: String,
    pub status: WalletStatus,
}
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct CoinEntry {
    #[schema(pattern = "^[1-9][0-9]{0,18}$")]
    pub id: String,
    pub operation_id: Uuid,
    pub kind: OperationKind,
    /// Public event category only. Internal notes, evidence and source IDs are excluded.
    pub source_type: String,
    #[schema(pattern = "^-?[1-9][0-9]{0,18}$")]
    pub delta: String,
    #[schema(pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub balance_after: String,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CoinEntryPage {
    pub items: Vec<CoinEntry>,
    pub pagination: PaginationMeta,
    /// Reuse this boundary on subsequent pages to exclude newly committed entries.
    #[schema(pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub snapshot: String,
}
#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct CoinEntriesQuery {
    #[param(default = 1, minimum = 1)]
    pub page: Option<u32>,
    #[param(default = 20, minimum = 1, maximum = 100)]
    pub page_size: Option<u32>,
    #[param(pattern = "^(0|[1-9][0-9]{0,18})$")]
    pub snapshot: Option<String>,
}
