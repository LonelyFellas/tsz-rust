mod coins_support;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use coins_support::*;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::{
    coins::{model::*, service},
    state::AppState,
};
use uuid::Uuid;
async fn admin(pool: &PgPool, super_admin: bool, keys: &[&str]) -> Owner {
    let owner = seed(pool, OwnerType::Admin, Uuid::now_v7()).await;
    if super_admin {
        sqlx::query("UPDATE admins SET role='super_admin' WHERE id=$1")
            .bind(owner.owner_id)
            .execute(pool)
            .await
            .unwrap();
    }
    for key in keys {
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,$2,$1)").bind(owner.owner_id).bind(key).execute(pool).await.unwrap();
    }
    owner
}
async fn request(
    state: &AppState,
    actor: Owner,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let token = state
        .admin_token_manager
        .generate(actor.owner_id, "admin")
        .unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/api/v1/admin{path}"))
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn input(owner: Owner, event: &str) -> Value {
    json!({"owner_type":owner.owner_type,"owner_id":owner.owner_id,"amount":"100","category":"purchase","event_id":event,"reason":"private reason","evidence_ref":"private evidence","idempotency_key":Uuid::new_v4()})
}
#[sqlx::test]
async fn permissions_identity_amounts_and_search(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let user = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let operator = admin(&pool, false, &["coins.access", "coins.credit"]).await;
    let other = admin(&pool, false, &[]).await;
    let root = admin(&pool, true, &[]).await;
    assert_eq!(
        request(
            &state,
            other,
            "POST",
            "/coins/manual-credits",
            input(user, "no-perm")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &state,
            operator,
            "POST",
            "/coins/manual-credits",
            input(other, "admin")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &state,
            root,
            "POST",
            "/coins/manual-credits",
            input(root, "self")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for amount in ["0", "-1", "01", "1.2", "9223372036854775808"] {
        let mut body = input(user, "bad");
        body["amount"] = json!(amount);
        assert_eq!(
            request(&state, operator, "POST", "/coins/manual-credits", body)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let (_, found) = request(
        &state,
        operator,
        "GET",
        &format!("/coins/accounts?owner_type=user&search={}", user.owner_id),
        Value::Null,
    )
    .await;
    assert_eq!(found["items"][0]["balance"], "0");
    assert!(found["items"][0].get("email").is_none());
    assert_eq!(
        request(
            &state,
            root,
            "POST",
            "/coins/manual-credits",
            input(other, "admin")
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, wallet) = request(&state, other, "GET", "/me/coins/wallet", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(wallet["balance"], "100");
    assert_eq!(balance(&pool, user).await, "0");
    reconciled(&pool).await;
}
#[sqlx::test]
async fn concurrent_credit_event_uniqueness_replay_and_atomic_audit(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let user = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let actor = admin(&pool, false, &["coins.access", "coins.credit"]).await;
    let other = admin(&pool, true, &[]).await;
    let body = input(user, "stable-purchase-1");
    let (a, b) = tokio::join!(
        request(&state, actor, "POST", "/coins/manual-credits", body.clone()),
        request(&state, actor, "POST", "/coins/manual-credits", body.clone())
    );
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(a, b);
    let mut changed = body.clone();
    changed["amount"] = json!("101");
    assert_eq!(
        request(&state, actor, "POST", "/coins/manual-credits", changed)
            .await
            .1["code"],
        "coin_idempotency_conflict"
    );
    let mut duplicate = input(user, "stable-purchase-1");
    duplicate["category"] = json!("reward");
    assert_eq!(
        request(&state, other, "POST", "/coins/manual-credits", duplicate)
            .await
            .1["code"],
        "coin_source_conflict"
    );
    assert_eq!(balance(&pool, user).await, "100");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit.admin_actions WHERE action='coins.credit'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    let (_, list) = request(
        &state,
        actor,
        "GET",
        "/coins/operations?source_type=manual_purchase",
        Value::Null,
    )
    .await;
    assert_eq!(list["items"][0]["reason"], "private reason");
    let public = tsz_rust::coins::repository::entries(
        &pool,
        user,
        tsz_rust::coins::dto::CoinEntriesQuery {
            page: None,
            page_size: None,
            snapshot: None,
        },
    )
    .await
    .unwrap();
    assert!(!serde_json::to_string(&public).unwrap().contains("private"));
    sqlx::query("CREATE FUNCTION fail_coin_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_coin_audit BEFORE INSERT ON audit.admin_actions FOR EACH ROW EXECUTE FUNCTION fail_coin_audit()").execute(&pool).await.unwrap();
    assert_eq!(
        request(
            &state,
            actor,
            "POST",
            "/coins/manual-credits",
            input(user, "rollback")
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(balance(&pool, user).await, "100");
    reconciled(&pool).await;
}
#[sqlx::test]
async fn reversal_is_full_once_private_and_permission_checked(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let user = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let actor = admin(&pool, false, &["coins.access", "coins.credit"]).await;
    let root = admin(&pool, true, &[]).await;
    let (_, original) = request(
        &state,
        actor,
        "POST",
        "/coins/manual-credits",
        input(user, "reverse"),
    )
    .await;
    let path = format!(
        "/coins/manual-credits/{}/reversal",
        original["id"].as_str().unwrap()
    );
    let body = json!({"idempotency_key":Uuid::new_v4(),"reason":"wrong recipient"});
    assert_eq!(
        request(&state, actor, "POST", &path, body.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    sqlx::raw_sql("CREATE FUNCTION fail_reversal_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='coins.reverse' THEN RAISE EXCEPTION 'synthetic failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_reversal_audit BEFORE INSERT ON audit.admin_actions FOR EACH ROW EXECUTE FUNCTION fail_reversal_audit();").execute(&pool).await.unwrap();
    assert_eq!(
        request(&state, root, "POST", &path, body.clone()).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(balance(&pool, user).await, "100");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM coin_operations WHERE reverses_operation_id IS NOT NULL"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    reconciled(&pool).await;
    sqlx::query("DROP TRIGGER fail_reversal_audit ON audit.admin_actions")
        .execute(&pool)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        request(&state, root, "POST", &path, body.clone()),
        request(&state, root, "POST", &path, body.clone())
    );
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(a, b);
    assert_eq!(a.1["delta"], "-100");
    assert_eq!(a.1["reverses_operation_id"], original["id"]);
    assert_eq!(
        request(
            &state,
            root,
            "POST",
            &path,
            json!({"idempotency_key":Uuid::new_v4(),"reason":"again"})
        )
        .await
        .1["code"],
        "coin_source_conflict"
    );
    assert_eq!(balance(&pool, user).await, "0");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit.admin_actions WHERE action='coins.reverse'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    let (_, issued) = request(
        &state,
        root,
        "POST",
        "/coins/manual-credits",
        input(actor, "admin-credit"),
    )
    .await;
    // The recipient cannot reverse their own wallet even after becoming super admin.
    sqlx::query("UPDATE admins SET role='super_admin' WHERE id=$1")
        .bind(actor.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(
            &state,
            actor,
            "POST",
            &format!(
                "/coins/manual-credits/{}/reversal",
                issued["id"].as_str().unwrap()
            ),
            body
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    reconciled(&pool).await;
}
#[sqlx::test]
async fn paused_wallet_rejects_credit_replay_and_reversal_and_insufficient_funds(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let user = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let root = admin(&pool, true, &[]).await;
    let body = input(user, "pending");
    let (_, original) = request(&state, root, "POST", "/coins/manual-credits", body.clone()).await;
    let path = format!(
        "/coins/manual-credits/{}/reversal",
        original["id"].as_str().unwrap()
    );
    let reverse = json!({"idempotency_key":Uuid::new_v4(),"reason":"error"});
    let mut tx = pool.begin().await.unwrap();
    service::pause_in(&mut tx, user).await.unwrap();
    tx.commit().await.unwrap();
    for payload in [body, input(user, "new-pending")] {
        assert_eq!(
            request(&state, root, "POST", "/coins/manual-credits", payload)
                .await
                .1["code"],
            "coin_wallet_unavailable"
        );
    }
    assert_eq!(
        request(&state, root, "POST", &path, reverse.clone())
            .await
            .1["code"],
        "coin_wallet_unavailable"
    );
    let mut tx = pool.begin().await.unwrap();
    service::resume_in(&mut tx, user).await.unwrap();
    service::debit_in(&mut tx, user, Amount::new(1).unwrap(), &context("spent"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        request(&state, root, "POST", &path, reverse).await.1["code"],
        "coin_insufficient_balance"
    );
    assert_eq!(balance(&pool, user).await, "99");
    reconciled(&pool).await;
}
#[sqlx::test]
async fn permission_revocation_while_waiting_is_rechecked(pool: PgPool) {
    let user = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let actor = admin(&pool, false, &["coins.access", "coins.credit"]).await;
    let mut revoke = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM admins WHERE id=$1 FOR UPDATE")
        .bind(actor.owner_id)
        .execute(&mut *revoke)
        .await
        .unwrap();
    let state = AppState::for_test(pool.clone());
    let pending = tokio::spawn(async move {
        request(
            &state,
            actor,
            "POST",
            "/coins/manual-credits",
            input(user, "revoked"),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5),async {loop {
        let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE cardinality(pg_blocking_pids(pid))>0 AND query LIKE '%FROM admins WHERE id = $1 FOR SHARE%')").fetch_one(&pool).await.unwrap();
        if waiting {break;} tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }}).await.unwrap();
    sqlx::query(
        "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='coins.credit'",
    )
    .bind(actor.owner_id)
    .execute(&mut *revoke)
    .await
    .unwrap();
    sqlx::query("UPDATE admins SET permission_version=permission_version+1 WHERE id=$1")
        .bind(actor.owner_id)
        .execute(&mut *revoke)
        .await
        .unwrap();
    revoke.commit().await.unwrap();
    assert_eq!(pending.await.unwrap().0, StatusCode::FORBIDDEN);
    assert_eq!(balance(&pool, user).await, "0");
    reconciled(&pool).await;
}

#[sqlx::test]
async fn manual_migration_roundtrip_and_nonmanual_reversal_rejected(pool: PgPool) {
    let down = include_str!("../migrations/20261006020000_manual_coins.down.sql");
    let up = include_str!("../migrations/20261006020000_manual_coins.up.sql");
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(down).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(up).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let user = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let original = credit(&pool, user, 10, "internal-reward").await;
    let actor = admin(&pool, true, &[]).await;
    let state = AppState::for_test(pool.clone());
    assert_eq!(
        request(
            &state,
            actor,
            "POST",
            &format!("/coins/manual-credits/{}/reversal", original.operation_id),
            json!({"idempotency_key":Uuid::new_v4(),"reason":"not manual"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &state,
            actor,
            "POST",
            "/coins/manual-credits",
            input(user, "migration-proof")
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(sqlx::raw_sql(down).execute(&mut *tx).await.is_err());
    tx.rollback().await.unwrap();
    assert_eq!(balance(&pool, user).await, "110");
    reconciled(&pool).await;
}

#[sqlx::test]
async fn distinct_operators_race_events_reversals_and_opposite_admin_targets(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let a = admin(&pool, true, &[]).await;
    let b = admin(&pool, true, &[]).await;
    let u = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let v = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let first = input(u, "racing-event");
    let mut second = input(v, "racing-event");
    second["category"] = json!("reward");
    let (one, two) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            request(&state, a, "POST", "/coins/manual-credits", first),
            request(&state, b, "POST", "/coins/manual-credits", second)
        )
    })
    .await
    .unwrap();
    let (success, failure) = if one.0 == StatusCode::OK {
        (one, two)
    } else {
        (two, one)
    };
    assert_eq!(success.0, StatusCode::OK);
    assert_eq!(failure.1["code"], "coin_source_conflict");
    assert_eq!(
        balance(&pool, u).await.parse::<i64>().unwrap()
            + balance(&pool, v).await.parse::<i64>().unwrap(),
        100
    );
    let path = format!(
        "/coins/manual-credits/{}/reversal",
        success.1["id"].as_str().unwrap()
    );
    let (one, two) = tokio::join!(
        request(
            &state,
            a,
            "POST",
            &path,
            json!({"idempotency_key":Uuid::new_v4(),"reason":"correction A"})
        ),
        request(
            &state,
            b,
            "POST",
            &path,
            json!({"idempotency_key":Uuid::new_v4(),"reason":"correction B"})
        )
    );
    assert_eq!(
        [one.0, two.0]
            .iter()
            .filter(|s| **s == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        if one.0 == StatusCode::OK {
            two.1
        } else {
            one.1
        }["code"],
        "coin_source_conflict"
    );
    assert_eq!(balance(&pool, u).await, "0");
    assert_eq!(balance(&pool, v).await, "0");
    let (one, two) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            request(
                &state,
                a,
                "POST",
                "/coins/manual-credits",
                input(b, "admin-a-to-b")
            ),
            request(
                &state,
                b,
                "POST",
                "/coins/manual-credits",
                input(a, "admin-b-to-a")
            )
        )
    })
    .await
    .unwrap();
    assert_eq!(one.0, StatusCode::OK, "{}", one.1);
    assert_eq!(two.0, StatusCode::OK, "{}", two.1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit.admin_actions WHERE resource_type='coin_operation'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        4
    );
    reconciled(&pool).await;
}

#[sqlx::test]
async fn password_change_during_account_lock_wait_revokes_manual_writes(pool: PgPool) {
    let mut outcomes = Vec::new();
    for reversing in [false, true] {
        let state = AppState::for_test(pool.clone());
        let user = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
        let actor = admin(
            &pool,
            false,
            &["coins.access", "coins.credit", "coins.reverse"],
        )
        .await;
        let (path, body) = if reversing {
            let original = request(
                &state,
                actor,
                "POST",
                "/coins/manual-credits",
                input(user, "before-password-change"),
            )
            .await;
            assert_eq!(original.0, StatusCode::OK);
            (
                format!(
                    "/coins/manual-credits/{}/reversal",
                    original.1["id"].as_str().unwrap()
                ),
                json!({"idempotency_key":Uuid::now_v7(),"reason":"correction"}),
            )
        } else {
            (
                "/coins/manual-credits".to_owned(),
                input(user, "after-password-change"),
            )
        };
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
            .bind(user.owner_id)
            .execute(&mut *blocker)
            .await
            .unwrap();
        let pending =
            tokio::spawn(async move { request(&state, actor, "POST", &path, body).await });
        tokio::time::timeout(std::time::Duration::from_secs(5),async {loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')").fetch_one(&pool).await.unwrap();
            if waiting {break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }}).await.unwrap();
        assert_eq!(
            tsz_rust::admin::AdminRepository::new(pool.clone())
                .set_password_if_unchanged(&actor.owner_id, "hash", "new-hash", false)
                .await
                .unwrap(),
            1
        );
        blocker.commit().await.unwrap();
        let result = pending.await.unwrap();
        let operations: i64 =
            sqlx::query_scalar("SELECT count(*) FROM coin_operations WHERE actor_id=$1")
                .bind(actor.owner_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let audits:i64=sqlx::query_scalar("SELECT count(*) FROM audit.admin_actions WHERE actor_admin_id=$1 AND resource_type='coin_operation'").bind(actor.owner_id).fetch_one(&pool).await.unwrap();
        outcomes.push((result.0, balance(&pool, user).await, operations, audits));
        reconciled(&pool).await;
    }
    assert_eq!(
        outcomes,
        vec![
            (StatusCode::UNAUTHORIZED, "0".to_owned(), 0, 0),
            (StatusCode::UNAUTHORIZED, "100".to_owned(), 1, 1)
        ]
    );
}

#[sqlx::test]
async fn password_change_during_admin_lock_wait_revokes_management_reads(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let user = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let actor = admin(&pool, false, &["coins.access"]).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM admins WHERE id=$1 FOR UPDATE")
        .bind(actor.owner_id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let state2 = state.clone();
    let accounts = tokio::spawn(async move {
        request(
            &state2,
            actor,
            "GET",
            &format!("/coins/accounts?owner_type=user&search={}", user.owner_id),
            Value::Null,
        )
        .await
    });
    let operations = tokio::spawn(async move {
        request(&state, actor, "GET", "/coins/operations", Value::Null).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5),async {loop {
        let waiting:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'").fetch_one(&pool).await.unwrap();
        if waiting>=2 {break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }}).await.unwrap();
    sqlx::query("UPDATE admins SET password_hash='new-hash',security_version=security_version+1 WHERE id=$1").bind(actor.owner_id).execute(&mut *blocker).await.unwrap();
    blocker.commit().await.unwrap();
    assert_eq!(accounts.await.unwrap().0, StatusCode::UNAUTHORIZED);
    assert_eq!(operations.await.unwrap().0, StatusCode::UNAUTHORIZED);
}
