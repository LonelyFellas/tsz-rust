mod account_deletion_support;
use account_deletion_support::*;
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use tsz_rust::{
    account_deletion::{dto::*, service},
    coins::{model::*, repository},
    otp::model::Purpose,
};
use uuid::Uuid;
#[sqlx::test]
async fn consent_exact_balance_and_legacy_client_cannot_bypass_wait(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    fund(&pool, &auth, 100).await;
    code(&state, &auth).await;
    let (status, body) = call(
        &state,
        &auth,
        "DELETE",
        "/api/v1/auth/account",
        json!({"channel":"email","code":"000000"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "account_deletion_upgrade_required");
    let key = Uuid::now_v7();
    let mut payload = input(key, "100");
    payload["waive_balance"] = json!(false);
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/account-deletion",
            payload
        )
        .await
        .1["code"],
        "account_deletion_consent_required"
    );
    let mut payload = input(key, "99");
    payload["waive_balance"] = json!(true);
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/account-deletion",
            payload
        )
        .await
        .1["code"],
        "account_deletion_balance_changed"
    );
    let (status, body) = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/account-deletion",
        input(key, "100"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["confirmed_balance"], "100");
    let wallet = repository::wallet(
        &pool,
        Owner {
            owner_type: OwnerType::User,
            owner_id: auth.subject,
        },
    )
    .await
    .unwrap();
    assert_eq!(wallet.status, WalletStatus::DeletionPending);
    assert_eq!(wallet.balance, "100");
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT EXTRACT(EPOCH FROM effective_at-requested_at)::bigint FROM account_deletion_requests").fetch_one(&pool).await.unwrap(),259200);
    let (_, saved) = call(
        &state,
        &auth,
        "GET",
        "/api/v1/me/account-deletion",
        json!({}),
    )
    .await;
    assert_eq!(saved["request"], body);
    assert_eq!(
        call(&state, &auth, "GET", "/api/v1/me", json!({})).await.0,
        StatusCode::OK
    );
}
#[sqlx::test]
async fn concurrent_retry_is_one_request_and_cancelled_key_never_reopens(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    code(&state, &auth).await;
    let key = Uuid::now_v7();
    let (a, b) = tokio::join!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/account-deletion",
            input(key, "0")
        ),
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/account-deletion",
            input(key, "0")
        )
    );
    assert_eq!(a.0, StatusCode::ACCEPTED, "{:?}", a.1);
    assert_eq!(a, b);
    let id = a.1["id"].as_str().unwrap();
    let path = format!("/api/v1/me/account-deletion/{id}/cancel");
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/account-deletion",
            input(Uuid::now_v7(), "0")
        )
        .await
        .1["code"],
        "account_deletion_pending"
    );
    let mut different = input(key, "0");
    different["waive_balance"] = json!(true);
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/account-deletion",
            different
        )
        .await
        .1["code"],
        "idempotency_conflict"
    );
    let cancelled = call(&state, &auth, "POST", &path, json!({})).await;
    assert_eq!(cancelled.0, StatusCode::OK);
    assert_eq!(cancelled.1["status"], "cancelled");
    assert_eq!(
        call(&state, &auth, "POST", &path, json!({})).await,
        cancelled
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/account-deletion",
            input(key, "0")
        )
        .await
        .1["status"],
        "cancelled"
    );
    assert_eq!(
        repository::wallet(
            &pool,
            Owner {
                owner_type: OwnerType::User,
                owner_id: auth.subject
            }
        )
        .await
        .unwrap()
        .status,
        WalletStatus::Open
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM coin_entries")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    // New intent needs a fresh OTP; an already consumed code cannot revive a request.
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/account-deletion",
            input(Uuid::now_v7(), "0")
        )
        .await
        .1["code"],
        "invalid_account_deletion_code"
    );
}
#[sqlx::test]
async fn bad_proof_missing_channel_and_foreign_cancel_have_no_effect(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    let (_, other) = setup(&pool).await;
    let mut payload = input(Uuid::now_v7(), "0");
    payload["channel"] = json!("phone");
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/account-deletion",
            payload
        )
        .await
        .1["code"],
        "account_deletion_channel_unavailable"
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/account-deletion",
            input(Uuid::now_v7(), "0")
        )
        .await
        .1["code"],
        "invalid_account_deletion_code"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM coin_wallets")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let req = apply(&state, &auth, "0").await;
    assert_eq!(
        call(
            &state,
            &other,
            "POST",
            &format!("/api/v1/me/account-deletion/{}/cancel", req.id),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
#[sqlx::test]
async fn cancel_preserves_balance_and_new_application_gets_new_deadline(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    fund(&pool, &auth, 9_007_199_254_740_993).await;
    let request = apply(&state, &auth, "9007199254740993").await;
    let cancelled = service::cancel(&state, &auth, request.id).await.unwrap();
    assert_eq!(cancelled.status, DeletionStatus::Cancelled);
    assert_eq!(
        repository::wallet(
            &pool,
            Owner {
                owner_type: OwnerType::User,
                owner_id: auth.subject
            }
        )
        .await
        .unwrap()
        .balance,
        "9007199254740993"
    );
    // Restore a new proof through the isolated OTP store; request cooldown is unrelated to lifecycle.
    let (proof_state, store) = tsz_rust::state::AppState::for_test_with_otp_store(pool.clone());
    store
        .save_code(
            &format!("{}@example.test", auth.subject),
            Purpose::AccountDeletion,
            "000000",
            std::time::Duration::from_secs(300),
        )
        .await
        .unwrap();
    let new = service::create(
        &proof_state,
        &auth,
        serde_json::from_value(input(Uuid::now_v7(), "9007199254740993")).unwrap(),
    )
    .await
    .unwrap();
    assert_ne!(new.id, request.id);
    assert!(new.effective_at > request.effective_at);
}

#[sqlx::test]
async fn deletion_code_remains_bound_to_current_contact_and_purpose(pool: PgPool) {
    let (_, auth) = setup(&pool).await;
    let (state, store) = tsz_rust::state::AppState::for_test_with_otp_store(pool.clone());
    let missing = call(
        &state,
        &auth,
        "POST",
        "/api/v1/auth/account/deletion-code",
        json!({"channel":"phone"}),
    )
    .await;
    assert_eq!(missing.0, StatusCode::CONFLICT);
    assert_eq!(missing.1["code"], "account_deletion_channel_unavailable");
    let sent = call(
        &state,
        &auth,
        "POST",
        "/api/v1/auth/account/deletion-code",
        json!({"channel":"email","purpose":"login","target":"attacker@example.test"}),
    )
    .await;
    assert_eq!(sent.0, StatusCode::ACCEPTED);
    let own = format!("{}@example.test", auth.subject);
    assert!(
        store
            .code_exists(&own, Purpose::AccountDeletion)
            .await
            .unwrap()
    );
    assert!(
        !store
            .code_exists("attacker@example.test", Purpose::AccountDeletion)
            .await
            .unwrap()
    );
    assert!(!store.code_exists(&own, Purpose::Login).await.unwrap());
}
