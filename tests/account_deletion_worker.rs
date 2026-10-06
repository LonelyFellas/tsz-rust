mod account_deletion_support;
use account_deletion_support::*;
use sqlx::PgPool;
use tsz_rust::{
    account_deletion::{service, worker},
    coins::repository,
};
#[sqlx::test]
async fn worker_waits_then_atomically_forfeits_deletes_and_retains_consent(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    fund(&pool, &auth, 100).await;
    let req = apply(&state, &auth, "100").await;
    assert_eq!(worker::sweep(&pool).await.unwrap(), 0);
    deadline(&pool, req.id, 0).await;
    // A restart needs no in-memory task. Multiple instances share the same persistent queue.
    let (a, b) = tokio::join!(worker::sweep(&pool), worker::sweep(&pool));
    assert_eq!(a.unwrap() + b.unwrap(), 1);
    assert_eq!(worker::sweep(&pool).await.unwrap(), 0);
    let state:(String,bool,String)=sqlx::query_as("SELECT d.status,EXISTS(SELECT 1 FROM users WHERE id=d.user_id),w.balance::text FROM account_deletion_requests d JOIN coin_wallets w ON w.owner_id=d.user_id AND w.owner_type='user' WHERE d.id=$1").bind(req.id).fetch_one(&pool).await.unwrap();
    assert_eq!(state, ("completed".into(), false, "0".into()));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM coin_operations WHERE kind='account_closure_forfeit'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    let r = repository::reconcile(&pool).await.unwrap();
    assert_eq!(r.total_balance, r.net_issuance);
    assert_eq!(r.wallet_mismatches, 0);
}
#[sqlx::test]
async fn cleanup_failure_rolls_back_forfeit_and_request_completion(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    fund(&pool, &auth, 10).await;
    let req = apply(&state, &auth, "10").await;
    deadline(&pool, req.id, -1).await;
    sqlx::raw_sql("CREATE FUNCTION refuse_deletion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected delete failure'; END $$; CREATE TRIGGER refuse_deletion BEFORE DELETE ON users FOR EACH ROW EXECUTE FUNCTION refuse_deletion();").execute(&pool).await.unwrap();
    assert!(
        service::complete(&pool, auth.subject, req.id)
            .await
            .is_err()
    );
    let row:(String,i64)=sqlx::query_as("SELECT d.status,w.balance FROM account_deletion_requests d JOIN coin_wallets w ON w.owner_id=d.user_id WHERE d.id=$1").bind(req.id).fetch_one(&pool).await.unwrap();
    assert_eq!(row, ("pending".into(), 10));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM coin_operations WHERE kind='account_closure_forfeit'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER refuse_deletion ON users")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        service::complete(&pool, auth.subject, req.id)
            .await
            .unwrap()
    );
}
#[sqlx::test]
async fn zero_balance_disabled_account_still_waits_and_can_complete(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    let req = apply(&state, &auth, "0").await;
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !service::complete(&pool, auth.subject, req.id)
            .await
            .unwrap()
    );
    deadline(&pool, req.id, -1).await;
    assert!(
        service::complete(&pool, auth.subject, req.id)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM coin_entries")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test]
async fn migration_rollback_preserves_even_cancelled_zero_balance_consent(pool: PgPool) {
    let down = include_str!("../migrations/20261006010000_account_deletion.down.sql");
    let up = include_str!("../migrations/20261006010000_account_deletion.up.sql");
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(down).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(up).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let (state, auth) = setup(&pool).await;
    let req = apply(&state, &auth, "0").await;
    service::cancel(&state, &auth, req.id).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(sqlx::raw_sql(down).execute(&mut *tx).await.is_err());
    tx.rollback().await.unwrap();
}

#[sqlx::test]
async fn busy_first_batch_yields_to_later_due_accounts(pool: PgPool) {
    let mut owners = Vec::new();
    for _ in 0..101 {
        let (state, auth) = setup(&pool).await;
        let req = apply(&state, &auth, "0").await;
        deadline(&pool, req.id, -1).await;
        owners.push(auth.subject);
    }
    let mut locks = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(&owners[..100])
        .execute(&mut *locks)
        .await
        .unwrap();
    assert_eq!(worker::sweep(&pool).await.unwrap(), 0);
    assert_eq!(
        worker::sweep(&pool).await.unwrap(),
        1,
        "busy first batch must not starve the 101st account"
    );
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1)")
            .bind(owners[100])
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    locks.rollback().await.unwrap();
}
