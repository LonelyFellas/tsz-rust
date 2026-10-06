//! All entry points require an existing transaction. Savepoints make an individual failed
//! operation atomic even if its caller elects to commit other work. Business authorization
//! belongs to the caller; include every party before taking any account lock.
//! Use at most one ordinary ledger operation per outer transaction. Multi-operation
//! batches need a complete account, idempotency and wallet lock plan, not repeated calls.
use super::model::*;
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection, Postgres, Transaction};
use uuid::Uuid;

/// Lock all accounts together in a stable order, before permissions::lock or business locks.
/// Supply the actor and both parties of one operation before taking other account locks.
pub async fn lock_accounts_in(
    tx: &mut Transaction<'_, Postgres>,
    owners: &[Owner],
) -> Result<(), CoinError> {
    let mut owners = owners.to_vec();
    owners.sort();
    owners.dedup();
    for owner in owners {
        lock_account(tx, owner, false).await?;
    }
    Ok(())
}
async fn lock_account(
    conn: &mut PgConnection,
    owner: Owner,
    exclusive: bool,
) -> Result<(), CoinError> {
    let query = match (owner.owner_type, exclusive) {
        (OwnerType::User, false) => "SELECT status = 'active' FROM users WHERE id = $1 FOR SHARE",
        (OwnerType::User, true) => "SELECT status = 'active' FROM users WHERE id = $1 FOR UPDATE",
        (OwnerType::Admin, false) => {
            "SELECT status = 'active' AND NOT must_change_password FROM admins WHERE id = $1 FOR SHARE"
        }
        (OwnerType::Admin, true) => {
            "SELECT status = 'active' AND NOT must_change_password FROM admins WHERE id = $1 FOR UPDATE"
        }
    };
    if sqlx::query_scalar::<_, bool>(query)
        .bind(owner.owner_id)
        .fetch_optional(conn)
        .await?
        != Some(true)
    {
        return Err(CoinError::AccountUnavailable);
    }
    Ok(())
}
async fn ensure_wallet(conn: &mut PgConnection, owner: Owner) -> Result<Uuid, CoinError> {
    sqlx::query("INSERT INTO coin_wallets (id,owner_type,owner_id) VALUES ($1,$2,$3) ON CONFLICT (owner_type,owner_id) DO NOTHING")
        .bind(Uuid::now_v7()).bind(owner.owner_type).bind(owner.owner_id).execute(&mut *conn).await?;
    Ok(
        sqlx::query_scalar("SELECT id FROM coin_wallets WHERE owner_type=$1 AND owner_id=$2")
            .bind(owner.owner_type)
            .bind(owner.owner_id)
            .fetch_one(conn)
            .await?,
    )
}
async fn lock_wallet(conn: &mut PgConnection, id: Uuid) -> Result<Wallet, CoinError> {
    Ok(sqlx::query_as(
        "SELECT id,owner_type,owner_id,balance,status FROM coin_wallets WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_one(conn)
    .await?)
}
fn validate(context: &Context) -> Result<(), CoinError> {
    for (value, max) in [
        (&context.idempotency_scope, 200),
        (&context.idempotency_key, 200),
        (&context.source_type, 100),
        (&context.source_id, 200),
    ] {
        if value.trim().is_empty() || value.chars().count() > max {
            return Err(CoinError::InvalidCommand);
        }
    }
    if context.reason.chars().count() > 1000
        || context
            .evidence_ref
            .as_ref()
            .is_some_and(|v| v.trim().is_empty() || v.chars().count() > 500)
    {
        return Err(CoinError::InvalidCommand);
    }
    Ok(())
}
async fn receipt(conn: &mut PgConnection, id: Uuid) -> Result<Receipt, CoinError> {
    let entries = sqlx::query_as("SELECT id,wallet_id,delta,balance_after FROM coin_entries WHERE operation_id=$1 ORDER BY id")
        .bind(id).fetch_all(conn).await?;
    Ok(Receipt {
        operation_id: id,
        entries,
    })
}
async fn advisory(conn: &mut PgConnection, values: &[&str]) -> Result<(), CoinError> {
    // JSON tuple encoding avoids delimiter collisions; hash collisions only serialize work.
    let key = serde_json::to_string(values).expect("string tuple");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(conn)
        .await?;
    Ok(())
}
async fn record(
    conn: &mut PgConnection,
    kind: OperationKind,
    changes: &[(Owner, i64)],
    context: &Context,
    closing: bool,
) -> Result<Receipt, CoinError> {
    validate(context)?;
    let hash = Sha256::digest(serde_json::to_vec(&(kind, changes, context)).expect("coin command"))
        .to_vec();
    advisory(
        conn,
        &[
            "coins-request",
            &context.idempotency_scope,
            &context.idempotency_key,
        ],
    )
    .await?;
    if let Some((id, old_hash)) = sqlx::query_as::<_, (Uuid,Vec<u8>)>("SELECT id,request_hash FROM coin_operations WHERE idempotency_scope=$1 AND idempotency_key=$2")
        .bind(&context.idempotency_scope).bind(&context.idempotency_key).fetch_optional(&mut *conn).await? {
        return if hash == old_hash { receipt(conn,id).await } else { Err(CoinError::IdempotencyConflict) };
    }
    advisory(
        conn,
        &["coins-source", &context.source_type, &context.source_id],
    )
    .await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM coin_operations WHERE source_type=$1 AND source_id=$2)",
    )
    .bind(&context.source_type)
    .bind(&context.source_id)
    .fetch_one(&mut *conn)
    .await?;
    if exists {
        return Err(CoinError::SourceConflict);
    }
    let mut ordered = changes.to_vec();
    ordered.sort_by_key(|(owner, _)| *owner);
    let mut ids = Vec::new();
    for (owner, delta) in ordered {
        ids.push((ensure_wallet(conn, owner).await?, delta));
    }
    ids.sort_by_key(|(id, _)| *id);
    let mut updates = Vec::new();
    for (id, delta) in ids {
        let wallet = lock_wallet(conn, id).await?;
        let expected = if closing {
            WalletStatus::DeletionPending
        } else {
            WalletStatus::Open
        };
        if wallet.status != expected {
            return Err(CoinError::WalletUnavailable);
        }
        let balance = wallet
            .balance
            .checked_add(delta)
            .ok_or(CoinError::BalanceOverflow)?;
        if balance < 0 {
            return Err(CoinError::InsufficientBalance);
        }
        updates.push((id, delta, balance));
    }
    let id = Uuid::now_v7();
    let (actor_type, actor_id) = match context.actor {
        Actor::System => ("system", None),
        Actor::Account(owner) => (owner.owner_type.as_str(), Some(owner.owner_id)),
    };
    sqlx::query("INSERT INTO coin_operations (id,kind,actor_type,actor_id,idempotency_scope,idempotency_key,request_hash,source_type,source_id,reason,evidence_ref) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
        .bind(id).bind(kind).bind(actor_type).bind(actor_id).bind(&context.idempotency_scope).bind(&context.idempotency_key).bind(hash)
        .bind(&context.source_type).bind(&context.source_id).bind(&context.reason).bind(&context.evidence_ref).execute(&mut *conn).await?;
    for (wallet_id, delta, balance) in updates {
        sqlx::query("UPDATE coin_wallets SET balance=$2,updated_at=clock_timestamp() WHERE id=$1")
            .bind(wallet_id)
            .bind(balance)
            .execute(&mut *conn)
            .await?;
        sqlx::query("INSERT INTO coin_entries (operation_id,wallet_id,delta,balance_after) VALUES ($1,$2,$3,$4)")
            .bind(id).bind(wallet_id).bind(delta).bind(balance).execute(&mut *conn).await?;
    }
    receipt(conn, id).await
}
async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    kind: OperationKind,
    changes: &[(Owner, i64)],
    context: &Context,
) -> Result<Receipt, CoinError> {
    let mut savepoint = tx.begin().await?;
    let result = async {
        let mut owners: Vec<_> = changes.iter().map(|(owner, _)| *owner).collect();
        if let Actor::Account(actor) = context.actor {
            owners.push(actor);
        }
        lock_accounts_in(&mut savepoint, &owners).await?;
        record(&mut savepoint, kind, changes, context, false).await
    }
    .await;
    match result {
        Ok(value) => {
            savepoint.commit().await?;
            Ok(value)
        }
        Err(error) => {
            savepoint.rollback().await?;
            Err(error)
        }
    }
}
pub async fn credit_in(
    tx: &mut Transaction<'_, Postgres>,
    owner: Owner,
    amount: Amount,
    context: &Context,
) -> Result<Receipt, CoinError> {
    apply(
        tx,
        OperationKind::Credit,
        &[(owner, amount.value())],
        context,
    )
    .await
}
pub async fn debit_in(
    tx: &mut Transaction<'_, Postgres>,
    owner: Owner,
    amount: Amount,
    context: &Context,
) -> Result<Receipt, CoinError> {
    apply(
        tx,
        OperationKind::Debit,
        &[(owner, -amount.value())],
        context,
    )
    .await
}
pub async fn transfer_in(
    tx: &mut Transaction<'_, Postgres>,
    from: Owner,
    to: Owner,
    amount: Amount,
    context: &Context,
) -> Result<Receipt, CoinError> {
    if from == to {
        return Err(CoinError::InvalidCommand);
    }
    apply(
        tx,
        OperationKind::Transfer,
        &[(from, -amount.value()), (to, amount.value())],
        context,
    )
    .await
}

/// Account-domain caller locks account → deletion request → wallet. Re-taking the
/// account lock here is safe, but the caller must never lock a request/wallet first.
pub async fn pause_in(
    tx: &mut Transaction<'_, Postgres>,
    owner: Owner,
) -> Result<Wallet, CoinError> {
    transition(tx, owner, WalletStatus::Open, WalletStatus::DeletionPending).await
}
pub async fn resume_in(
    tx: &mut Transaction<'_, Postgres>,
    owner: Owner,
) -> Result<Wallet, CoinError> {
    transition(tx, owner, WalletStatus::DeletionPending, WalletStatus::Open).await
}
async fn transition(
    tx: &mut Transaction<'_, Postgres>,
    owner: Owner,
    from: WalletStatus,
    to: WalletStatus,
) -> Result<Wallet, CoinError> {
    let mut savepoint = tx.begin().await?;
    let result = async {
        lock_account(&mut savepoint, owner, true).await?;
        let id = ensure_wallet(&mut savepoint, owner).await?;
        let mut wallet = lock_wallet(&mut savepoint, id).await?;
        if wallet.status != from && wallet.status != to {
            return Err(CoinError::InvalidTransition);
        }
        sqlx::query("UPDATE coin_wallets SET status=$2,updated_at=clock_timestamp() WHERE id=$1")
            .bind(id)
            .bind(to)
            .execute(&mut *savepoint)
            .await?;
        wallet.status = to;
        Ok(wallet)
    }
    .await;
    match result {
        Ok(value) => {
            savepoint.commit().await?;
            Ok(value)
        }
        Err(error) => {
            savepoint.rollback().await?;
            Err(error)
        }
    }
}
/// Only the future account-deletion worker may call this after checking consent and
/// the 72-hour deadline. It must atomically delete the account and complete the request.
/// Zero balance closes without manufacturing a zero entry.
pub async fn close_in(
    tx: &mut Transaction<'_, Postgres>,
    owner: Owner,
    confirmed_balance: i64,
    deletion_request_id: Uuid,
) -> Result<(), CoinError> {
    let mut savepoint = tx.begin().await?;
    let result = async {
        // Account disablement must not prevent an already-consented closure.
        let query = match owner.owner_type {
            OwnerType::User => "SELECT id FROM users WHERE id=$1 FOR UPDATE",
            OwnerType::Admin => "SELECT id FROM admins WHERE id=$1 FOR UPDATE",
        };
        if sqlx::query_scalar::<_, Uuid>(query)
            .bind(owner.owner_id)
            .fetch_optional(&mut *savepoint)
            .await?
            .is_none()
        {
            return Err(CoinError::AccountUnavailable);
        }
        let id = ensure_wallet(&mut savepoint, owner).await?;
        let wallet = lock_wallet(&mut savepoint, id).await?;
        if wallet.status == WalletStatus::Closed {
            return Ok(());
        }
        if wallet.status != WalletStatus::DeletionPending {
            return Err(CoinError::InvalidTransition);
        }
        if confirmed_balance != wallet.balance {
            return Err(CoinError::BalanceChanged);
        }
        if wallet.balance > 0 {
            let context = Context {
                actor: Actor::System,
                idempotency_scope: "account_closure".into(),
                idempotency_key: deletion_request_id.to_string(),
                source_type: "account_closure".into(),
                source_id: deletion_request_id.to_string(),
                reason: "Account closure after consent".into(),
                evidence_ref: None,
            };
            record(
                &mut savepoint,
                OperationKind::AccountClosureForfeit,
                &[(owner, -wallet.balance)],
                &context,
                true,
            )
            .await?;
        }
        sqlx::query(
            "UPDATE coin_wallets SET status='closed',updated_at=clock_timestamp() WHERE id=$1",
        )
        .bind(id)
        .execute(&mut *savepoint)
        .await?;
        Ok(())
    }
    .await;
    match result {
        Ok(value) => {
            savepoint.commit().await?;
            Ok(value)
        }
        Err(error) => {
            savepoint.rollback().await?;
            Err(error)
        }
    }
}
