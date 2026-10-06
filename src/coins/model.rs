use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum OwnerType {
    User,
    Admin,
}
impl OwnerType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Admin => "admin",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Owner {
    pub owner_type: OwnerType,
    pub owner_id: Uuid,
}
#[derive(Debug, Clone, Copy, Serialize)]
pub enum Actor {
    Account(Owner),
    System,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum WalletStatus {
    Open,
    DeletionPending,
    Closed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum OperationKind {
    Credit,
    Debit,
    Transfer,
    AccountClosureForfeit,
}
impl OperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Credit => "credit",
            Self::Debit => "debit",
            Self::Transfer => "transfer",
            Self::AccountClosureForfeit => "account_closure_forfeit",
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Amount(i64);
impl Amount {
    pub fn new(value: i64) -> Result<Self, CoinError> {
        if value > 0 {
            Ok(Self(value))
        } else {
            Err(CoinError::InvalidAmount)
        }
    }
    pub fn value(self) -> i64 {
        self.0
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Context {
    pub actor: Actor,
    pub idempotency_scope: String,
    pub idempotency_key: String,
    pub source_type: String,
    pub source_id: String,
    /// Internal only; never projected to personal entries.
    pub reason: String,
    pub evidence_ref: Option<String>,
}
#[derive(Debug, Clone, FromRow)]
pub struct Wallet {
    pub id: Uuid,
    pub owner_type: OwnerType,
    pub owner_id: Uuid,
    pub balance: i64,
    pub status: WalletStatus,
}
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Entry {
    pub id: i64,
    pub wallet_id: Uuid,
    pub delta: i64,
    pub balance_after: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub operation_id: Uuid,
    pub entries: Vec<Entry>,
}
#[derive(Debug, thiserror::Error)]
pub enum CoinError {
    #[error("amount must be a positive i64")]
    InvalidAmount,
    #[error("invalid operation context or same-wallet transfer")]
    InvalidCommand,
    #[error("account missing or inactive")]
    AccountUnavailable,
    #[error("wallet is not open")]
    WalletUnavailable,
    #[error("invalid wallet lifecycle transition")]
    InvalidTransition,
    #[error("confirmed balance changed")]
    BalanceChanged,
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("balance overflow")]
    BalanceOverflow,
    #[error("idempotency payload conflict")]
    IdempotencyConflict,
    #[error("business event already recorded")]
    SourceConflict,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}
