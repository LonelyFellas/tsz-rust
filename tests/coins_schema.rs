mod coins_support;
use coins_support::*;
use sqlx::PgPool;
use tsz_rust::coins::{model::*, service};
use uuid::Uuid;
const UP: &str = include_str!("../migrations/20261006000000_coins.up.sql");
const DOWN: &str = include_str!("../migrations/20261006000000_coins.down.sql");
#[sqlx::test]
async fn empty_roundtrip_and_constraints(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(DOWN).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(UP).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let owner = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    credit(&pool, owner, 10, "initial").await;
    for statement in [
        "UPDATE coin_wallets SET balance=-1",
        "UPDATE coin_wallets SET status='closed'",
        "UPDATE coin_wallets SET owner_type='teacher'",
        "UPDATE coin_entries SET delta=0",
        "UPDATE coin_entries SET balance_after=-1",
        "DELETE FROM coin_wallets",
        "DELETE FROM coin_operations",
        "INSERT INTO coin_wallets (id,owner_type,owner_id) SELECT gen_random_uuid(),owner_type,owner_id FROM coin_wallets",
        "INSERT INTO coin_entries (wallet_id,operation_id,delta,balance_after) SELECT wallet_id,operation_id,delta,balance_after FROM coin_entries",
        "INSERT INTO coin_operations SELECT gen_random_uuid(),kind,actor_type,actor_id,idempotency_scope,idempotency_key,request_hash,source_type,source_id,reason,evidence_ref,created_at FROM coin_operations",
    ] {
        assert!(
            sqlx::query(statement).execute(&pool).await.is_err(),
            "{statement}"
        );
    }
    let mut tx = pool.begin().await.unwrap();
    assert!(sqlx::raw_sql(DOWN).execute(&mut *tx).await.is_err());
    tx.rollback().await.unwrap();
    assert_eq!(balance(&pool, owner).await, "10");
    reconciled(&pool).await;
}
#[sqlx::test]
async fn down_keeps_zero_balance_lifecycle_evidence(pool: PgPool) {
    let owner = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let mut tx = pool.begin().await.unwrap();
    service::pause_in(&mut tx, owner).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(sqlx::raw_sql(DOWN).execute(&mut *tx).await.is_err());
    tx.rollback().await.unwrap();
    assert_eq!(balance(&pool, owner).await, "0");
    reconciled(&pool).await;
}
