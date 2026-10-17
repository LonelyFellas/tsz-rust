mod account_deletion_support;
mod coins_support;
use account_deletion_support::call;
use coins_support::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use tsz_rust::{auth::extract::AuthUser, coins::model::*, state::AppState};
use uuid::Uuid;

const PATH: &str = "/api/v1/me/coins/login-reward";
async fn setup(pool: &PgPool) -> (AppState, AuthUser, Owner) {
    let owner = seed(pool, OwnerType::User, Uuid::now_v7()).await;
    enable_policy(pool).await;
    let mut state = AppState::for_test(pool.clone());
    state.login_reward_enabled = true;
    (
        state,
        AuthUser {
            subject: owner.owner_id,
            role: "student".into(),
            security_version: 0,
        },
        owner,
    )
}
async fn enable_policy(pool: &PgPool) {
    let mut tx = pool.begin().await.unwrap();
    // Isolated fixture: emulate the common policy having been scheduled yesterday.
    sqlx::query(
        "ALTER TABLE learning_reward_policies DISABLE TRIGGER learning_reward_policy_schedule",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("INSERT INTO learning_reward_policies(id,rule_version,effective_business_day,enabled,daily_amount,minimum_units) SELECT gen_random_uuid(),'login-fixture',((clock_timestamp() AT TIME ZONE 'Asia/Shanghai')-interval '4 hours')::date,true,5,10 WHERE NOT EXISTS(SELECT 1 FROM learning_reward_policies)").execute(&mut *tx).await.unwrap();
    sqlx::query(
        "ALTER TABLE learning_reward_policies ENABLE TRIGGER learning_reward_policy_schedule",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
async fn claim(state: &AppState, auth: &AuthUser) -> Value {
    let r = call(state, auth, "POST", PATH, json!({})).await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}

#[sqlx::test]
async fn concurrent_visits_devices_and_roles_issue_one_coin(pool: PgPool) {
    let (state, auth, owner) = setup(&pool).await;
    let (a, b, c) = tokio::join!(
        claim(&state, &auth),
        claim(&state, &auth),
        claim(&state, &auth)
    );
    assert_eq!(a["awarded_amount"], "1");
    assert_eq!(a["status"], "awarded");
    assert_eq!(a["operation_id"], b["operation_id"]);
    assert_eq!(a["operation_id"], c["operation_id"]);
    let teacher = AuthUser {
        subject: auth.subject,
        role: "teacher".into(),
        security_version: 0,
    };
    let r = tsz_rust::coins::login_reward::settle(&pool, &teacher, true)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(r).unwrap()["operation_id"],
        a["operation_id"]
    );
    assert_eq!(balance(&pool, owner).await, "1");
    let (_, _, other) = setup(&pool).await;
    assert_eq!(balance(&pool, other).await, "0");
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM coin_operations WHERE source_type='daily_login_reward'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
    reconciled(&pool).await;
}

#[sqlx::test]
async fn rejects_client_authority_unbound_and_revoked_accounts(pool: PgPool) {
    let (state, auth, owner) = setup(&pool).await;
    for body in [
        json!({"amount":"100"}),
        json!({"business_day":"2026-01-01"}),
        json!({"user_id":Uuid::now_v7()}),
    ] {
        assert_eq!(call(&state, &auth, "POST", PATH, body).await.0, 422);
    }
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &format!("{PATH}?amount=100"),
            json!({})
        )
        .await
        .0,
        400
    );
    assert_eq!(call(&state, &auth, "GET", PATH, Value::Null).await.0, 405);
    sqlx::query("UPDATE users SET phone=NULL WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(call(&state, &auth, "POST", PATH, json!({})).await.0, 403);
    sqlx::query("UPDATE users SET security_version=1 WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(call(&state, &auth, "POST", PATH, json!({})).await.0, 401);
    assert_eq!(balance(&pool, owner).await, "0");
    // Account lock wait must re-check the session, not trust the extractor's old read.
    sqlx::query("UPDATE users SET phone='19800000001',security_version=0 WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(auth.subject)
        .execute(&mut *lock)
        .await
        .unwrap();
    let release = async {
        wait_for_lock(&pool).await;
        sqlx::query("UPDATE users SET security_version=1 WHERE id=$1")
            .bind(auth.subject)
            .execute(&mut *lock)
            .await
            .unwrap();
        lock.commit().await.unwrap();
    };
    let (r, ()) = tokio::join!(call(&state, &auth, "POST", PATH, json!({})), release);
    assert_eq!(r.0, 401, "{}", r.1);
    assert_eq!(balance(&pool, owner).await, "0");
}

async fn wait_for_lock(pool: &PgPool) {
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')").fetch_one(pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
}

#[sqlx::test]
async fn pending_skips_until_recovery_and_closure_preserves_ledger(pool: PgPool) {
    let (mut state, auth) = account_deletion_support::setup_bound(&pool).await;
    state.login_reward_enabled = true;
    enable_policy(&pool).await;
    let request = account_deletion_support::apply(&state, &auth, "0").await;
    assert_eq!(claim(&state, &auth).await["status"], "wallet_unavailable");
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &format!("/api/v1/me/account-deletion/{}/cancel", request.id),
            json!({})
        )
        .await
        .0,
        200
    );
    let first = claim(&state, &auth).await;
    assert_eq!(first["awarded_amount"], "1");
    let mut state = AppState::for_test(pool.clone());
    state.login_reward_enabled = true;
    let request = account_deletion_support::apply(&state, &auth, "1").await;
    assert_eq!(
        claim(&state, &auth).await["operation_id"],
        first["operation_id"]
    );
    account_deletion_support::deadline(&pool, request.id, -1).await;
    assert_eq!(call(&state, &auth, "POST", PATH, json!({})).await.0, 401);
    assert!(
        tsz_rust::account_deletion::service::complete(&pool, auth.subject, request.id)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM coin_operations WHERE source_type='daily_login_reward'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    reconciled(&pool).await;
}

#[sqlx::test]
async fn server_day_boundary_wallet_wait_and_failed_write_are_atomic(pool: PgPool) {
    sqlx::raw_sql("CREATE SCHEMA reward_clock; CREATE TABLE reward_clock.instant(at timestamptz NOT NULL); INSERT INTO reward_clock.instant VALUES('2026-10-17T19:59:59Z'); CREATE FUNCTION reward_clock.clock_timestamp() RETURNS timestamptz LANGUAGE sql VOLATILE AS 'SELECT at FROM reward_clock.instant';").execute(&pool).await.unwrap();
    let timed = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .after_connect(|c, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO reward_clock,public,pg_catalog")
                    .execute(c)
                    .await?;
                Ok(())
            })
        })
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let (state, auth, owner) = setup(&timed).await;
    let first = claim(&state, &auth).await;
    assert_eq!(first["business_day"], "2026-10-17");
    sqlx::query("UPDATE reward_clock.instant SET at='2026-10-17T20:00:00Z'")
        .execute(&pool)
        .await
        .unwrap();
    let second = claim(&state, &auth).await;
    assert_eq!(second["business_day"], "2026-10-18");
    assert_ne!(first["operation_id"], second["operation_id"]);
    assert_eq!(balance(&pool, owner).await, "2");
    let (other, b, b_owner) = setup(&timed).await;
    credit(&pool, b_owner, 1, "wallet-fixture").await;
    sqlx::query("UPDATE reward_clock.instant SET at='2026-10-18T19:59:59Z'")
        .execute(&pool)
        .await
        .unwrap();
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM coin_wallets WHERE owner_id=$1 FOR UPDATE")
        .bind(b.subject)
        .execute(&mut *lock)
        .await
        .unwrap();
    let release = async {
        wait_for_lock(&pool).await;
        sqlx::query("UPDATE reward_clock.instant SET at='2026-10-18T20:00:00Z'")
            .execute(&pool)
            .await
            .unwrap();
        lock.commit().await.unwrap();
    };
    let (r, ()) = tokio::join!(call(&other, &b, "POST", PATH, json!({})), release);
    assert_eq!(r.0, 409, "{}", r.1);
    assert_eq!(balance(&pool, b_owner).await, "1");
    assert_eq!(claim(&other, &b).await["business_day"], "2026-10-19");
    assert_eq!(balance(&pool, b_owner).await, "2");
    let (other, c, c_owner) = setup(&timed).await;
    sqlx::raw_sql("CREATE FUNCTION fail_login_entry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture entry failure'; END; $$; CREATE TRIGGER fail_login_entry BEFORE INSERT ON coin_entries FOR EACH ROW EXECUTE FUNCTION fail_login_entry();").execute(&pool).await.unwrap();
    assert_eq!(call(&other, &c, "POST", PATH, json!({})).await.0, 500);
    assert_eq!(balance(&pool, c_owner).await, "0");
    sqlx::query("DROP TRIGGER fail_login_entry ON coin_entries")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(claim(&other, &c).await["awarded_amount"], "1");
    reconciled(&pool).await;
    timed.close().await;
}

#[sqlx::test]
async fn server_switch_blocks_new_awards_and_preserves_existing_receipts(pool: PgPool) {
    let (mut state, auth, owner) = setup(&pool).await;
    state.login_reward_enabled = false;
    assert_eq!(claim(&state, &auth).await["status"], "reward_disabled");
    assert_eq!(balance(&pool, owner).await, "0");
    state.login_reward_enabled = true;
    let awarded = claim(&state, &auth).await;
    assert_eq!(awarded["awarded_amount"], "1");
    state.login_reward_enabled = false;
    assert_eq!(
        claim(&state, &auth).await["operation_id"],
        awarded["operation_id"]
    );
    let (_, other, other_owner) = setup(&pool).await;
    assert_eq!(claim(&state, &other).await["status"], "reward_disabled");
    assert_eq!(balance(&pool, other_owner).await, "0");
    reconciled(&pool).await;
}
