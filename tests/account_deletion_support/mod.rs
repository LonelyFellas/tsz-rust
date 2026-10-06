#![allow(dead_code)]
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::{
    account_deletion::{dto::*, service},
    auth::extract::AuthUser,
    coins::{model::*, service as coins},
    otp::model::Purpose,
    state::AppState,
};
use uuid::Uuid;
pub async fn setup(pool: &PgPool) -> (AppState, AuthUser) {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users(id,email,password_hash,display_name) VALUES ($1,$2,'hash','Deletion')",
    )
    .bind(id)
    .bind(format!("{id}@example.test"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO user_roles(user_id,role) VALUES ($1,'student')")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    (
        AppState::for_test(pool.clone()),
        AuthUser {
            subject: id,
            role: "student".into(),
            security_version: 0,
        },
    )
}
pub async fn setup_bound(pool: &PgPool) -> (AppState, AuthUser) {
    let (state, auth) = setup(pool).await;
    sqlx::query("UPDATE users SET phone=$2 WHERE id=$1")
        .bind(auth.subject)
        .bind(format!("199{:08}", auth.subject.as_u128() % 100_000_000))
        .execute(pool)
        .await
        .unwrap();
    (state, auth)
}
pub fn input(key: Uuid, balance: &str) -> Value {
    json!({"channel":"email","code":"000000","expected_coin_balance":balance,"waive_balance":balance!="0","confirm_deletion":true,"consent_version":service::CONSENT_VERSION,"idempotency_key":key})
}
pub async fn code(state: &AppState, auth: &AuthUser) {
    state
        .otp_service
        .request(
            &format!("{}@example.test", auth.subject),
            Purpose::AccountDeletion,
        )
        .await
        .unwrap();
}
pub async fn call(
    state: &AppState,
    auth: &AuthUser,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let token = state
        .token_manager
        .generate_with_version(auth.subject, "student", auth.security_version)
        .unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("Authorization", format!("Bearer {}", token))
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
pub async fn apply(state: &AppState, auth: &AuthUser, balance: &str) -> AccountDeletionRequest {
    code(state, auth).await;
    service::create(
        state,
        auth,
        serde_json::from_value(input(Uuid::now_v7(), balance)).unwrap(),
    )
    .await
    .unwrap()
}
pub async fn deadline(pool: &PgPool, id: Uuid, offset_ms: i64) {
    let at: DateTime<Utc> =
        sqlx::query_scalar("SELECT clock_timestamp()+$1*interval '1 millisecond'")
            .bind(offset_ms)
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::query("UPDATE account_deletion_requests SET effective_at=$2,requested_at=$2-interval '72 hours',signed_at=$2-interval '72 hours' WHERE id=$1").bind(id).bind(at).execute(pool).await.unwrap();
}
pub async fn fund(pool: &PgPool, auth: &AuthUser, amount: i64) {
    let key = Uuid::now_v7().to_string();
    let ctx = Context {
        actor: Actor::System,
        idempotency_scope: "test".into(),
        idempotency_key: key.clone(),
        source_type: "test_reward".into(),
        source_id: key,
        reason: "test".into(),
        evidence_ref: None,
    };
    let mut tx = pool.begin().await.unwrap();
    coins::credit_in(
        &mut tx,
        Owner {
            owner_type: OwnerType::User,
            owner_id: auth.subject,
        },
        Amount::new(amount).unwrap(),
        &ctx,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
pub async fn blocked(pool: &PgPool, pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if sqlx::query_scalar::<_, bool>("SELECT cardinality(pg_blocking_pids($1))>0")
                .bind(pid)
                .fetch_one(pool)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
