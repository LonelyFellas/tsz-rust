use super::{dto::*, model::*};
use crate::{
    api::PaginationMeta,
    error::{AppError, ErrorCode},
};
use sqlx::PgPool;

pub async fn wallet(pool: &PgPool, owner: Owner) -> Result<CoinWallet, sqlx::Error> {
    let stored: Option<(i64, WalletStatus)> = sqlx::query_as(
        "SELECT balance,status FROM coin_wallets WHERE owner_type=$1 AND owner_id=$2",
    )
    .bind(owner.owner_type)
    .bind(owner.owner_id)
    .fetch_optional(pool)
    .await?;
    let (balance, status) = stored.unwrap_or((0, WalletStatus::Open));
    Ok(CoinWallet {
        owner_type: owner.owner_type,
        owner_id: owner.owner_id,
        balance: balance.to_string(),
        status,
    })
}
pub async fn entries(
    pool: &PgPool,
    owner: Owner,
    query: CoinEntriesQuery,
) -> Result<CoinEntryPage, AppError> {
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);
    let invalid = || AppError::bad_request(ErrorCode::InvalidQuery, "invalid coin pagination");
    if page == 0 || !(1..=100).contains(&page_size) {
        return Err(invalid());
    }
    let boundary = match query.snapshot {
        None => None,
        Some(value) => {
            let parsed = value.parse::<i64>().map_err(|_| invalid())?;
            if parsed < 0 || parsed.to_string() != value {
                return Err(invalid());
            }
            Some(parsed)
        }
    };
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let wallet_id: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT id FROM coin_wallets WHERE owner_type=$1 AND owner_id=$2")
            .bind(owner.owner_type)
            .bind(owner.owner_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    let snapshot = match boundary {
        Some(v) => v,
        None => sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(max(id),0) FROM coin_entries WHERE wallet_id=$1",
        )
        .bind(wallet_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?,
    };
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM coin_entries WHERE wallet_id=$1 AND id<=$2")
            .bind(wallet_id)
            .bind(snapshot)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    let items=sqlx::query_as("SELECT e.id::text AS id,e.operation_id,o.kind,o.source_type,e.delta::text AS delta,e.balance_after::text AS balance_after,e.created_at FROM coin_entries e JOIN coin_operations o ON o.id=e.operation_id WHERE e.wallet_id=$1 AND e.id<=$2 ORDER BY e.id DESC LIMIT $3 OFFSET $4")
        .bind(wallet_id).bind(snapshot).bind(i64::from(page_size)).bind(i64::from(page-1)*i64::from(page_size))
        .fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(CoinEntryPage {
        items,
        pagination: PaginationMeta {
            page,
            page_size,
            total,
            total_pages: (total + i64::from(page_size) - 1) / i64::from(page_size),
        },
        snapshot: snapshot.to_string(),
    })
}
#[derive(Debug, sqlx::FromRow)]
pub struct Reconciliation {
    pub wallet_mismatches: i64,
    pub running_balance_mismatches: i64,
    pub operation_mismatches: i64,
    pub total_balance: String,
    pub net_issuance: String,
}
/// One read-only statement gives all checks the same MVCC snapshot, including closed wallets.
pub async fn reconcile(pool: &PgPool) -> Result<Reconciliation, sqlx::Error> {
    sqlx::query_as(include_str!("reconciliation.sql"))
        .fetch_one(pool)
        .await
}
