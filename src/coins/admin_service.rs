use super::{admin_dto::*, model::*, service};
use crate::{
    admin::permissions,
    api::PaginationMeta,
    error::{AppError, ErrorCode},
};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

pub fn coin_error(error: CoinError) -> AppError {
    let code = match error {
        CoinError::Database(error) => {
            if error
                .as_database_error()
                .is_some_and(|e| e.constraint() == Some("coin_manual_event_once"))
            {
                return AppError::conflict(
                    ErrorCode::CoinSourceConflict,
                    None,
                    "event already recorded",
                );
            }
            return AppError::internal(error);
        }
        CoinError::InvalidAmount | CoinError::BalanceOverflow => ErrorCode::CoinInvalidAmount,
        CoinError::InvalidCommand => ErrorCode::InvalidRequestBody,
        CoinError::AccountUnavailable => ErrorCode::CoinAccountUnavailable,
        CoinError::WalletUnavailable | CoinError::InvalidTransition => {
            ErrorCode::CoinWalletUnavailable
        }
        CoinError::InsufficientBalance => ErrorCode::CoinInsufficientBalance,
        CoinError::IdempotencyConflict => ErrorCode::CoinIdempotencyConflict,
        CoinError::SourceConflict => ErrorCode::CoinSourceConflict,
        CoinError::BalanceChanged => ErrorCode::AccountDeletionBalanceChanged,
    };
    if matches!(
        code,
        ErrorCode::CoinInvalidAmount | ErrorCode::InvalidRequestBody
    ) {
        AppError::bad_request(code, "invalid coin request")
    } else {
        AppError::conflict(code, None, "coin operation rejected")
    }
}
fn text(value: &str, max: usize) -> Result<(), AppError> {
    if value.trim().is_empty() || value.chars().count() > max || value.trim() != value {
        return Err(AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "text must be nonempty, trimmed and within limit",
        ));
    }
    Ok(())
}
async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    owner: Owner,
    key: &str,
) -> Result<(), AppError> {
    service::lock_accounts_in(
        tx,
        &[
            Owner {
                owner_type: OwnerType::Admin,
                owner_id: actor,
            },
            owner,
        ],
    )
    .await
    .map_err(coin_error)?;
    let authorization = permissions::lock(tx, actor).await?;
    authorization.require(key)?;
    if owner.owner_type == OwnerType::Admin
        && (owner.owner_id == actor || !authorization.is_super_admin)
    {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "only a super admin may operate another admin wallet",
        ));
    }
    // Account locks serialize lifecycle changes, including replay attempts while paused.
    let status: Option<WalletStatus> =
        sqlx::query_scalar("SELECT status FROM coin_wallets WHERE owner_type=$1 AND owner_id=$2")
            .bind(owner.owner_type)
            .bind(owner.owner_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    if status.is_some_and(|s| s != WalletStatus::Open) {
        return Err(coin_error(CoinError::WalletUnavailable));
    }
    Ok(())
}
// SQL fragments below are fixed literals selected by typed enums; all request values are bound.
const PROJECTION: &str = "SELECT o.id,w.owner_type,w.owner_id,o.actor_id,o.source_type,o.source_id AS event_id,e.delta::text AS delta,e.balance_after::text AS balance_after,o.reason,o.evidence_ref,o.reverses_operation_id,r.id AS reversed_by_operation_id,o.created_at FROM coin_operations o JOIN coin_entries e ON e.operation_id=o.id JOIN coin_wallets w ON w.id=e.wallet_id LEFT JOIN coin_operations r ON r.reverses_operation_id=o.id";
async fn operation(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<ManualCoinOperation, AppError> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{PROJECTION} WHERE o.id=$1 AND o.source_type IN ('manual_purchase','manual_reward','manual_reversal')")))
        .bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("manual operation not found"))
}
async fn audit_once(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    key: Uuid,
    id: Uuid,
    action: &str,
) -> Result<(), AppError> {
    // The ledger request lock is still held, so successful retries cannot duplicate audit.
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM audit.admin_actions WHERE resource_type='coin_operation' AND resource_id=$1 AND action=$2)")
        .bind(id).bind(action).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
    if !exists {
        permissions::service::audit(
            tx,
            actor,
            key,
            action,
            "coin_operation",
            id,
            serde_json::json!({"operation_id": id}),
        )
        .await?;
    }
    Ok(())
}
pub async fn credit(
    pool: &PgPool,
    actor: Uuid,
    input: ManualCreditRequest,
) -> Result<ManualCoinOperation, AppError> {
    text(&input.event_id, 200)?;
    text(&input.reason, 1000)?;
    if let Some(evidence) = &input.evidence_ref {
        text(evidence, 500)?;
    }
    let value = input
        .amount
        .parse::<i64>()
        .map_err(|_| coin_error(CoinError::InvalidAmount))?;
    if value.to_string() != input.amount {
        return Err(coin_error(CoinError::InvalidAmount));
    }
    let amount = Amount::new(value).map_err(coin_error)?;
    let owner = Owner {
        owner_type: input.owner_type,
        owner_id: input.owner_id,
    };
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    authorize(&mut tx, actor, owner, "coins.credit").await?;
    let context = Context {
        actor: Actor::Account(Owner {
            owner_type: OwnerType::Admin,
            owner_id: actor,
        }),
        idempotency_scope: format!("manual_credit:{actor}"),
        idempotency_key: input.idempotency_key.to_string(),
        source_type: input.category.source_type().into(),
        source_id: input.event_id,
        reason: input.reason,
        evidence_ref: input.evidence_ref,
    };
    let receipt = service::credit_in(&mut tx, owner, amount, &context)
        .await
        .map_err(coin_error)?;
    audit_once(
        &mut tx,
        actor,
        input.idempotency_key,
        receipt.operation_id,
        "coins.credit",
    )
    .await?;
    let response = operation(&mut tx, receipt.operation_id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(response)
}
pub async fn reverse(
    pool: &PgPool,
    actor: Uuid,
    original_id: Uuid,
    input: ManualReversalRequest,
) -> Result<ManualCoinOperation, AppError> {
    text(&input.reason, 1000)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    // Immutable original data determines every affected party before any account lock.
    let original = operation(&mut tx, original_id).await?;
    if !matches!(
        original.source_type.as_str(),
        "manual_purchase" | "manual_reward"
    ) {
        return Err(AppError::conflict(
            ErrorCode::CoinReversalInvalid,
            None,
            "only an original manual credit can be reversed",
        ));
    }
    let owner = Owner {
        owner_type: original.owner_type,
        owner_id: original.owner_id,
    };
    authorize(&mut tx, actor, owner, "coins.reverse").await?;
    let amount =
        Amount::new(original.delta.parse().map_err(AppError::internal)?).map_err(coin_error)?;
    let context = Context {
        actor: Actor::Account(Owner {
            owner_type: OwnerType::Admin,
            owner_id: actor,
        }),
        idempotency_scope: format!("manual_reversal:{actor}"),
        idempotency_key: input.idempotency_key.to_string(),
        source_type: "manual_reversal".into(),
        source_id: original_id.to_string(),
        reason: input.reason,
        evidence_ref: None,
    };
    let receipt = service::debit_in(&mut tx, owner, amount, &context)
        .await
        .map_err(coin_error)?;
    sqlx::query("UPDATE coin_operations SET reverses_operation_id=$2 WHERE id=$1 AND reverses_operation_id IS NULL")
        .bind(receipt.operation_id).bind(original_id).execute(&mut *tx).await.map_err(AppError::internal)?;
    audit_once(
        &mut tx,
        actor,
        input.idempotency_key,
        receipt.operation_id,
        "coins.reverse",
    )
    .await?;
    let response = operation(&mut tx, receipt.operation_id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(response)
}
fn pagination(page: Option<u32>, size: Option<u32>) -> Result<(u32, u32), AppError> {
    let (page, size) = (page.unwrap_or(1), size.unwrap_or(20));
    if page == 0 || !(1..=100).contains(&size) {
        return Err(AppError::bad_request(
            ErrorCode::InvalidQuery,
            "invalid pagination",
        ));
    }
    Ok((page, size))
}
fn meta(page: u32, page_size: u32, total: i64) -> PaginationMeta {
    PaginationMeta {
        page,
        page_size,
        total,
        total_pages: (total + i64::from(page_size) - 1) / i64::from(page_size),
    }
}
pub async fn accounts(
    pool: &PgPool,
    actor: Uuid,
    query: CoinAccountsQuery,
) -> Result<CoinAccountPage, AppError> {
    let (page, size) = pagination(query.page, query.page_size)?;
    if query.search.trim().is_empty() || query.search.chars().count() > 200 {
        return Err(AppError::bad_request(
            ErrorCode::InvalidQuery,
            "account search required",
        ));
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    permissions::lock(&mut tx, actor)
        .await?
        .require("coins.access")?;
    let (table, email) = match query.owner_type {
        OwnerType::User => ("users", "a.email"),
        OwnerType::Admin => ("admins", "NULL::text"),
    };
    let from = format!(
        "FROM {table} a LEFT JOIN coin_wallets w ON w.owner_type=$1 AND w.owner_id=a.id WHERE (a.id::text=$2 OR a.phone=$2 OR lower({email})=lower($2) OR strpos(lower(a.display_name),lower($2))>0)"
    );
    let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) {from}")))
        .bind(query.owner_type)
        .bind(query.search.trim())
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let items=sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT $1::text AS owner_type,a.id AS owner_id,a.display_name,a.status AS account_status,COALESCE(w.balance,0)::text AS balance,COALESCE(w.status,'open') AS status {from} ORDER BY a.id LIMIT $3 OFFSET $4")))
        .bind(query.owner_type).bind(query.search.trim()).bind(i64::from(size)).bind(i64::from(page-1)*i64::from(size)).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(CoinAccountPage {
        items,
        pagination: meta(page, size, total),
    })
}
pub async fn operations(
    pool: &PgPool,
    actor: Uuid,
    query: CoinOperationsQuery,
) -> Result<ManualCoinOperationPage, AppError> {
    let (page, size) = pagination(query.page, query.page_size)?;
    if query.source_type.as_ref().is_some_and(|s| {
        !matches!(
            s.as_str(),
            "manual_purchase" | "manual_reward" | "manual_reversal"
        )
    }) || matches!((query.created_from,query.created_to),(Some(a),Some(b)) if a>b)
    {
        return Err(AppError::bad_request(
            ErrorCode::InvalidQuery,
            "invalid operation filters",
        ));
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    permissions::lock(&mut tx, actor)
        .await?
        .require("coins.access")?;
    let filter = " WHERE o.source_type IN ('manual_purchase','manual_reward','manual_reversal') AND ($1::text IS NULL OR w.owner_type=$1) AND ($2::uuid IS NULL OR w.owner_id=$2) AND ($3::uuid IS NULL OR o.actor_id=$3) AND ($4::text IS NULL OR o.source_type=$4) AND ($5::timestamptz IS NULL OR o.created_at>=$5) AND ($6::timestamptz IS NULL OR o.created_at<=$6)";
    let total:i64=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM coin_operations o JOIN coin_entries e ON e.operation_id=o.id JOIN coin_wallets w ON w.id=e.wallet_id {filter}")))
        .bind(query.owner_type).bind(query.owner_id).bind(query.actor_id).bind(&query.source_type).bind(query.created_from).bind(query.created_to).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    let items = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{PROJECTION} {filter} ORDER BY o.created_at DESC,o.id DESC LIMIT $7 OFFSET $8"
    )))
    .bind(query.owner_type)
    .bind(query.owner_id)
    .bind(query.actor_id)
    .bind(&query.source_type)
    .bind(query.created_from)
    .bind(query.created_to)
    .bind(i64::from(size))
    .bind(i64::from(page - 1) * i64::from(size))
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(ManualCoinOperationPage {
        items,
        pagination: meta(page, size, total),
    })
}
