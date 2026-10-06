mod account_deletion_support;
use account_deletion_support::{apply, call, deadline, fund, setup_bound};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::{auth::extract::AuthUser, otp::model::Purpose, state::AppState};

const PASSWORD: &str = "Violet!River7294Cloud";
async fn register(state: &AppState, email: &str, code: &str) -> (StatusCode, Value) {
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/register")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({"email":email,"code":"000000","password":PASSWORD,"invite_code":code})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn otp(state: &AppState, email: &str) {
    state
        .otp_service
        .request(email, Purpose::Register)
        .await
        .unwrap();
}
async fn invite_code(state: &AppState, auth: &AuthUser) -> String {
    let result = call(
        state,
        auth,
        "POST",
        "/api/v1/me/invitations/code",
        Value::Null,
    )
    .await;
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    result.1["invite_code"].as_str().unwrap().to_owned()
}
async fn wallet(state: &AppState, auth: &AuthUser) -> String {
    call(state, auth, "GET", "/api/v1/me/coins/wallet", Value::Null)
        .await
        .1["balance"]
        .as_str()
        .unwrap()
        .to_owned()
}
async fn records(state: &AppState, auth: &AuthUser) -> Value {
    let result = call(
        state,
        auth,
        "GET",
        "/api/v1/me/invitations/records",
        Value::Null,
    )
    .await;
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    result.1
}
#[sqlx::test]
async fn code_is_stable_rewards_are_atomic_and_records_are_private(pool: PgPool) {
    let (mut state, auth) = setup_bound(&pool).await;
    state.invitation_reward_amount = Some(9_007_199_254_740_993);
    let overview = call(&state, &auth, "GET", "/api/v1/me/invitations", Value::Null)
        .await
        .1;
    assert!(overview["invite_code"].is_null());
    let (a, b) = tokio::join!(invite_code(&state, &auth), invite_code(&state, &auth));
    assert_eq!(a, b);
    otp(&state, "invitee@example.test").await;
    let (status, registered) = register(&state, "invitee@example.test", &a.to_lowercase()).await;
    assert_eq!(status, StatusCode::CREATED, "{registered}");
    assert_eq!(wallet(&state, &auth).await, "9007199254740993");
    let list = records(&state, &auth).await;
    assert_eq!(list["pagination"]["total"], 1);
    assert_eq!(list["items"][0]["reward_status"], "awarded");
    assert_eq!(list["items"][0]["reward_amount"], "9007199254740993");
    assert_eq!(
        list["items"][0]["invitee_user_id"],
        registered["user"]["id"]
    );
    assert!(!list.to_string().contains("invitee@example.test"));
    let (_, other) = setup_bound(&pool).await;
    assert_eq!(records(&state, &other).await["pagination"]["total"], 0);
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            "/api/v1/me/invitations/records?page=0",
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let entries = call(
        &state,
        &auth,
        "GET",
        "/api/v1/me/coins/entries",
        Value::Null,
    )
    .await
    .1;
    assert_eq!(entries["items"][0]["source_type"], "invitation_reward");
    assert!(
        register(&state, "invitee@example.test", &a)
            .await
            .0
            .is_client_error()
    );
    assert_eq!(wallet(&state, &auth).await, "9007199254740993");
    let report = tsz_rust::coins::repository::reconcile(&pool).await.unwrap();
    assert_eq!(report.wallet_mismatches, 0);
    assert_eq!(report.running_balance_mismatches, 0);
    assert_eq!(report.operation_mismatches, 0);
}
#[sqlx::test]
async fn invalid_code_preserves_otp_and_disabled_rewards_never_backfill(pool: PgPool) {
    let (mut state, auth) = setup_bound(&pool).await;
    let code = invite_code(&state, &auth).await;
    otp(&state, "disabled@example.test").await;
    let invalid = register(&state, "disabled@example.test", "FFFFFFFFFFFFFFFF").await;
    assert_eq!(invalid.0, StatusCode::BAD_REQUEST);
    assert_eq!(invalid.1["code"], "invalid_invitation_code");
    assert_eq!(
        register(&state, "disabled@example.test", &code).await.0,
        StatusCode::CREATED
    );
    assert_eq!(wallet(&state, &auth).await, "0");
    state.invitation_reward_amount = Some(19);
    assert_eq!(
        records(&state, &auth).await["items"][0]["reward_status"],
        "reward_disabled"
    );
    assert_eq!(wallet(&state, &auth).await, "0");
    otp(&state, "plain@example.test").await;
    assert_eq!(
        register(&state, "plain@example.test", "").await.0,
        StatusCode::CREATED
    );
    assert_eq!(records(&state, &auth).await["pagination"]["total"], 1);
}
#[sqlx::test]
async fn inactive_pending_expired_and_deleted_inviters_do_not_block_registration(pool: PgPool) {
    let (mut state, auth) = setup_bound(&pool).await;
    state.invitation_reward_amount = Some(23);
    let code = invite_code(&state, &auth).await;
    let request = apply(&state, &auth, "0").await;
    for email in [
        "pending@example.test",
        "expired@example.test",
        "deleted@example.test",
    ] {
        if email.starts_with("expired") {
            deadline(&pool, request.id, -1).await;
        }
        if email.starts_with("deleted") {
            tsz_rust::account_deletion::service::complete(&pool, auth.subject, request.id)
                .await
                .unwrap();
        }
        otp(&state, email).await;
        assert_eq!(register(&state, email, &code).await.0, StatusCode::CREATED);
    }
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT reward_status FROM invitation_registrations")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(statuses, vec!["inviter_unavailable"; 3]);
    let (_, other) = setup_bound(&pool).await;
    let other_code = invite_code(&state, &other).await;
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(other.subject)
        .execute(&pool)
        .await
        .unwrap();
    otp(&state, "inactive@example.test").await;
    assert_eq!(
        register(&state, "inactive@example.test", &other_code)
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM coin_operations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test]
async fn concurrent_registration_cannot_credit_two_inviters(pool: PgPool) {
    let (mut first, a) = setup_bound(&pool).await;
    let (mut second, b) = setup_bound(&pool).await;
    first.invitation_reward_amount = Some(13);
    second.invitation_reward_amount = Some(13);
    let ca = invite_code(&first, &a).await;
    let cb = invite_code(&second, &b).await;
    otp(&first, "same@example.test").await;
    otp(&second, "same@example.test").await;
    let (ra, rb) = tokio::join!(
        register(&first, "same@example.test", &ca),
        register(&second, "same@example.test", &cb)
    );
    let mut statuses = [ra.0.as_u16(), rb.0.as_u16()];
    statuses.sort();
    assert_eq!(statuses, [201, 409]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM invitation_registrations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        wallet(&first, &a).await.parse::<i64>().unwrap()
            + wallet(&second, &b).await.parse::<i64>().unwrap(),
        13
    );
}
#[sqlx::test]
async fn technical_failures_roll_back_registration_and_reward(pool: PgPool) {
    let (mut state, auth) = setup_bound(&pool).await;
    state.invitation_reward_amount = Some(29);
    let code = invite_code(&state, &auth).await;
    for table in ["coin_entries", "refresh_tokens"] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE FUNCTION reject_invitation_test() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test failure'; END $$; CREATE TRIGGER reject_invitation_test BEFORE INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION reject_invitation_test();"))).execute(&pool).await.unwrap();
        let email = format!("{table}@example.test");
        otp(&state, &email).await;
        assert!(register(&state, &email, &code).await.0.is_server_error());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users WHERE email=$1")
                .bind(&email)
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(wallet(&state, &auth).await, "0");
        assert_eq!(records(&state, &auth).await["pagination"]["total"], 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM coin_operations")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TRIGGER reject_invitation_test ON {table}; DROP FUNCTION reject_invitation_test();"))).execute(&pool).await.unwrap();
    }
    fund(&pool, &auth, i64::MAX).await;
    otp(&state, "overflow@example.test").await;
    assert!(
        register(&state, "overflow@example.test", &code)
            .await
            .0
            .is_server_error()
    );
    assert_eq!(wallet(&state, &auth).await, i64::MAX.to_string());
    assert_eq!(records(&state, &auth).await["pagination"]["total"], 0);
}
#[sqlx::test]
async fn waiting_registration_observes_inviter_state_after_lock(pool: PgPool) {
    let (mut state, auth) = setup_bound(&pool).await;
    state.invitation_reward_amount = Some(31);
    let code = invite_code(&state, &auth).await;
    otp(&state, "waiting@example.test").await;
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(auth.subject)
        .execute(&mut *lock)
        .await
        .unwrap();
    let cloned = state.clone();
    let task = tokio::spawn(async move { register(&cloned, "waiting@example.test", &code).await });
    tokio::time::timeout(std::time::Duration::from_secs(10),async {
        loop {
            let blocked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT status=%FOR SHARE')").fetch_one(&pool).await.unwrap();
            if blocked {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(auth.subject)
        .execute(&mut *lock)
        .await
        .unwrap();
    lock.commit().await.unwrap();
    assert_eq!(task.await.unwrap().0, StatusCode::CREATED);
    let status: String = sqlx::query_scalar("SELECT reward_status FROM invitation_registrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "inviter_unavailable");
}

#[sqlx::test]
async fn code_creation_rechecks_deadline_after_unique_key_wait(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let request = apply(&state, &auth, "0").await;
    let mut gate = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO invitation_codes(user_id,code) VALUES($1,'0123456789ABCDEF')")
        .bind(auth.subject)
        .execute(&mut *gate)
        .await
        .unwrap();
    let cloned = state.clone();
    let other = AuthUser {
        subject: auth.subject,
        role: auth.role.clone(),
        security_version: auth.security_version,
    };
    let task = tokio::spawn(async move {
        call(
            &cloned,
            &other,
            "POST",
            "/api/v1/me/invitations/code",
            Value::Null,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'INSERT INTO invitation_codes%')").fetch_one(&pool).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    deadline(&pool, request.id, -1).await;
    gate.rollback().await.unwrap();
    assert_eq!(task.await.unwrap().0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM invitation_codes")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
