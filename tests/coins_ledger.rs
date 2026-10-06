mod coins_support;
use coins_support::*;
use sqlx::PgPool;
use tsz_rust::coins::{model::*, repository, service::*};
use uuid::Uuid;

#[sqlx::test]
async fn identity_precision_and_outer_transaction_rollback(pool: PgPool) {
    let id = Uuid::now_v7();
    let user = seed(&pool, OwnerType::User, id).await;
    let admin = seed(&pool, OwnerType::Admin, id).await;
    assert_eq!(balance(&pool, user).await, "0");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM coin_wallets")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert!(Amount::new(0).is_err());
    assert!(Amount::new(-1).is_err());
    credit(&pool, user, i64::MAX, "max").await;
    credit(&pool, admin, 7, "admin").await;
    let mut tx = pool.begin().await.unwrap();
    assert!(matches!(
        credit_in(&mut tx, user, Amount::new(1).unwrap(), &context("overflow")).await,
        Err(CoinError::BalanceOverflow)
    ));
    tx.commit().await.unwrap();
    assert_eq!(balance(&pool, user).await, i64::MAX.to_string());
    assert_eq!(balance(&pool, admin).await, "7");
    let mut tx = pool.begin().await.unwrap();
    debit_in(&mut tx, user, Amount::new(1).unwrap(), &context("rollback"))
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(balance(&pool, user).await, i64::MAX.to_string());
    reconciled(&pool).await;
}

#[sqlx::test]
async fn concurrent_retries_open_once_and_conflicts_have_no_effect(pool: PgPool) {
    let owner = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            credit(&pool, owner, 100, "same").await
        }));
    }
    let mut receipts = Vec::new();
    for task in tasks {
        receipts.push(task.await.unwrap());
    }
    assert!(receipts.iter().all(|r| r == &receipts[0]));
    assert_eq!(balance(&pool, owner).await, "100");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM coin_wallets")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let other = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    for variant in 0..6 {
        let mut command = context("same");
        let mut target = owner;
        let mut value = 100;
        match variant {
            0 => value = 101,
            1 => target = other,
            2 => command.reason = "changed".into(),
            3 => command.evidence_ref = None,
            4 => command.source_id = "changed".into(),
            _ => command.actor = Actor::Account(other),
        }
        let mut tx = pool.begin().await.unwrap();
        assert!(matches!(
            credit_in(&mut tx, target, Amount::new(value).unwrap(), &command).await,
            Err(CoinError::IdempotencyConflict)
        ));
        tx.commit().await.unwrap();
    }
    let mut duplicate = context("changed-key");
    duplicate.source_id = "same".into();
    let mut tx = pool.begin().await.unwrap();
    assert!(matches!(
        credit_in(&mut tx, owner, Amount::new(100).unwrap(), &duplicate).await,
        Err(CoinError::SourceConflict)
    ));
    tx.commit().await.unwrap();
    assert_eq!(balance(&pool, other).await, "0");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM coin_operations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    reconciled(&pool).await;
}

#[sqlx::test]
async fn concurrent_business_dedup_and_debits_cannot_overspend(pool: PgPool) {
    let owner = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let mut tasks = Vec::new();
    for i in 0..8 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            let mut command = context(&format!("issue-{i}"));
            command.source_id = "one-event".into();
            let result = credit_in(&mut tx, owner, Amount::new(100).unwrap(), &command).await;
            tx.commit().await.unwrap();
            result
        }));
    }
    let mut successes = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => successes += 1,
            Err(e) => assert!(matches!(e, CoinError::SourceConflict)),
        }
    }
    assert_eq!(successes, 1);
    let mut tasks = Vec::new();
    for i in 0..12 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            let result = debit_in(
                &mut tx,
                owner,
                Amount::new(30).unwrap(),
                &context(&format!("spend-{i}")),
            )
            .await;
            tx.commit().await.unwrap();
            result
        }));
    }
    let mut successes = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => successes += 1,
            Err(e) => assert!(matches!(e, CoinError::InsufficientBalance)),
        }
    }
    assert_eq!(successes, 3);
    assert_eq!(balance(&pool, owner).await, "10");
    reconciled(&pool).await;
}

#[sqlx::test]
async fn transfer_atomicity_opposite_direction_and_replay(pool: PgPool) {
    let a = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let b = seed(&pool, OwnerType::Admin, Uuid::now_v7()).await;
    credit(&pool, a, 100, "a").await;
    credit(&pool, b, 100, "b").await;
    let mut tx = pool.begin().await.unwrap();
    let receipt = transfer_in(&mut tx, a, b, Amount::new(30).unwrap(), &context("tip"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(receipt.entries.len(), 2);
    assert_eq!(receipt.entries.iter().map(|e| e.delta).sum::<i64>(), 0);
    assert_eq!(balance(&pool, a).await, "70");
    assert_eq!(balance(&pool, b).await, "130");
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        transfer_in(&mut tx, a, b, Amount::new(30).unwrap(), &context("tip"))
            .await
            .unwrap(),
        receipt
    );
    assert!(matches!(
        transfer_in(
            &mut tx,
            a,
            b,
            Amount::new(71).unwrap(),
            &context("insufficient")
        )
        .await,
        Err(CoinError::InsufficientBalance)
    ));
    assert!(matches!(
        transfer_in(&mut tx, a, a, Amount::new(1).unwrap(), &context("self")).await,
        Err(CoinError::InvalidCommand)
    ));
    tx.commit().await.unwrap();
    let mut tasks = Vec::new();
    for i in 0..10 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            let (from, to) = if i % 2 == 0 { (a, b) } else { (b, a) };
            let mut tx = pool.begin().await.unwrap();
            transfer_in(
                &mut tx,
                from,
                to,
                Amount::new(1).unwrap(),
                &context(&format!("opposite-{i}")),
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        for task in tasks {
            task.await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(balance(&pool, a).await, "70");
    assert_eq!(balance(&pool, b).await, "130");
    reconciled(&pool).await;
}

#[sqlx::test]
async fn statement_failure_rolls_back_both_sides_even_when_caller_commits(pool: PgPool) {
    let a = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let b = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    credit(&pool, a, 100, "fund").await;
    // Fail on the second entry after the first balance/entry have already been written.
    sqlx::raw_sql("CREATE FUNCTION fail_second_coin_entry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF EXISTS (SELECT 1 FROM coin_entries WHERE operation_id=NEW.operation_id) THEN RAISE EXCEPTION 'injected second-entry failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_second BEFORE INSERT ON coin_entries FOR EACH ROW EXECUTE FUNCTION fail_second_coin_entry();").execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(matches!(
        transfer_in(&mut tx, a, b, Amount::new(30).unwrap(), &context("fail")).await,
        Err(CoinError::Database(_))
    ));
    tx.commit().await.unwrap();
    assert_eq!(balance(&pool, a).await, "100");
    assert_eq!(balance(&pool, b).await, "0");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM coin_operations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    reconciled(&pool).await;
}

#[sqlx::test]
async fn pause_resume_and_close_preserve_ledger_and_reject_all_ordinary_flows(pool: PgPool) {
    let a = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let b = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    credit(&pool, a, 100, "fund").await;
    credit(&pool, b, 10, "fund-b").await;
    let mut tx = pool.begin().await.unwrap();
    pause_in(&mut tx, a).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(matches!(
        credit_in(&mut tx, a, Amount::new(1).unwrap(), &context("paused-in")).await,
        Err(CoinError::WalletUnavailable)
    ));
    assert!(matches!(
        debit_in(&mut tx, a, Amount::new(1).unwrap(), &context("paused-out")).await,
        Err(CoinError::WalletUnavailable)
    ));
    for (from, to) in [(a, b), (b, a)] {
        assert!(matches!(
            transfer_in(
                &mut tx,
                from,
                to,
                Amount::new(1).unwrap(),
                &context("paused-transfer")
            )
            .await,
            Err(CoinError::WalletUnavailable)
        ));
    }
    assert!(matches!(
        close_in(&mut tx, a, 99, Uuid::now_v7()).await,
        Err(CoinError::BalanceChanged)
    ));
    tx.commit().await.unwrap();
    assert_eq!(balance(&pool, a).await, "100");
    let mut tx = pool.begin().await.unwrap();
    resume_in(&mut tx, a).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(balance(&pool, a).await, "100");
    let mut tx = pool.begin().await.unwrap();
    pause_in(&mut tx, a).await.unwrap();
    let request = Uuid::now_v7();
    close_in(&mut tx, a, 100, request).await.unwrap();
    close_in(&mut tx, a, 100, request).await.unwrap();
    assert!(matches!(
        resume_in(&mut tx, a).await,
        Err(CoinError::InvalidTransition)
    ));
    tx.commit().await.unwrap();
    assert_eq!(balance(&pool, a).await, "0");
    assert_eq!(
        repository::wallet(&pool, a).await.unwrap().status,
        WalletStatus::Closed
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM coin_operations WHERE kind='account_closure_forfeit'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    let zero = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let mut tx = pool.begin().await.unwrap();
    pause_in(&mut tx, zero).await.unwrap();
    close_in(&mut tx, zero, 0, Uuid::now_v7()).await.unwrap();
    tx.commit().await.unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(a.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM coin_operations WHERE kind='account_closure_forfeit'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    reconciled(&pool).await;
}

#[sqlx::test]
async fn disabled_deleted_and_pending_accounts_serialize_with_writes(pool: PgPool) {
    for action in ["disable", "delete", "pause"] {
        let owner = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
        let mut gate = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
            .bind(owner.owner_id)
            .execute(&mut *gate)
            .await
            .unwrap();
        let p = pool.clone();
        let key = action.to_owned();
        let (started, pid) = tokio::sync::oneshot::channel();
        let waiting = tokio::spawn(async move {
            let mut tx = p.begin().await.unwrap();
            let backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            started.send(backend_pid).unwrap();
            let result = credit_in(&mut tx, owner, Amount::new(1).unwrap(), &context(&key)).await;
            tx.commit().await.unwrap();
            result
        });
        wait_until_blocked(&pool, pid.await.unwrap()).await;
        match action {
            "disable" => {
                sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
                    .bind(owner.owner_id)
                    .execute(&mut *gate)
                    .await
                    .unwrap();
            }
            "delete" => {
                sqlx::query("DELETE FROM users WHERE id=$1")
                    .bind(owner.owner_id)
                    .execute(&mut *gate)
                    .await
                    .unwrap();
            }
            _ => {
                pause_in(&mut gate, owner).await.unwrap();
            }
        }
        gate.commit().await.unwrap();
        let result = waiting.await.unwrap();
        if action == "pause" {
            assert!(matches!(result, Err(CoinError::WalletUnavailable)));
        } else {
            assert!(matches!(result, Err(CoinError::AccountUnavailable)));
        }
        assert_eq!(balance(&pool, owner).await, "0");
    }
    reconciled(&pool).await;
}

#[sqlx::test]
async fn disablement_does_not_block_consented_pending_closure(pool: PgPool) {
    let owner = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    credit(&pool, owner, 100, "fund").await;
    let mut tx = pool.begin().await.unwrap();
    pause_in(&mut tx, owner).await.unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(owner.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    close_in(&mut tx, owner, 100, Uuid::now_v7()).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(balance(&pool, owner).await, "0");
    assert_eq!(
        repository::wallet(&pool, owner).await.unwrap().status,
        WalletStatus::Closed
    );
    reconciled(&pool).await;
}

async fn wait_until_blocked(pool: &PgPool, pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT cardinality(pg_blocking_pids($1)) > 0")
                .bind(pid)
                .fetch_one(pool)
                .await
                .unwrap();
            if blocked {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("database must observe the in-flight operation waiting on the account lock");
}

#[sqlx::test]
async fn in_flight_credit_blocks_account_deletion_and_ledger_survives(pool: PgPool) {
    let owner = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let mut posting = pool.begin().await.unwrap();
    credit_in(
        &mut posting,
        owner,
        Amount::new(10).unwrap(),
        &context("before-delete"),
    )
    .await
    .unwrap();
    let p = pool.clone();
    let (started, pid) = tokio::sync::oneshot::channel();
    let deletion = tokio::spawn(async move {
        let mut tx = p.begin().await.unwrap();
        let backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        started.send(backend_pid).unwrap();
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(owner.owner_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    });
    wait_until_blocked(&pool, pid.await.unwrap()).await;
    posting.commit().await.unwrap();
    deletion.await.unwrap();
    assert_eq!(balance(&pool, owner).await, "10");
    let mut tx = pool.begin().await.unwrap();
    assert!(matches!(
        credit_in(
            &mut tx,
            owner,
            Amount::new(1).unwrap(),
            &context("after-delete")
        )
        .await,
        Err(CoinError::AccountUnavailable)
    ));
    tx.commit().await.unwrap();
    reconciled(&pool).await;
}
