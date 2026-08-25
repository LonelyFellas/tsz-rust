//! `POST /api/v1/auth/register` 端到端测试（真 PG + 真 Redis）。
//!
//! 当前契约：仅手机号注册；请求包含 `phone/password/code`；验证码用途固定为
//! `register`；成功后直接返回登录响应并下发 HttpOnly refresh cookie。

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

use tsz_rust::otp::{model::Purpose, store::OtpStore};
use tsz_rust::state::AppState;

const PHONE: &str = "13800138000";
const PASSWORD: &str = "password123";
const CODE: &str = "123456";

fn ttl() -> Duration {
    Duration::from_secs(300)
}

async fn save_register_code(store: &OtpStore, phone: &str) {
    store
        .save_code(phone, Purpose::Register, CODE, ttl())
        .await
        .expect("测试验证码应写入成功");
}

async fn register(state: &AppState, body: Value) -> (StatusCode, Option<String>, Value) {
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/register")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .map(|value| value.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, cookie, body)
}

#[sqlx::test]
async fn valid_phone_code_registers_and_issues_session(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    save_register_code(&store, PHONE).await;

    let (status, cookie, body) = register(
        &state,
        json!({"phone": PHONE, "password": PASSWORD, "code": CODE}),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert!(
        body["access_token"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "注册成功应直接返回 access token"
    );
    assert_eq!(body["user"]["phone"], PHONE);
    assert_eq!(body["user"]["active_role"], "student");
    assert!(body["refresh_token_expires_at"].as_i64().is_some());
    assert!(body.get("password").is_none());
    assert!(body.get("password_hash").is_none());

    let cookie = cookie.expect("注册成功应下发 refresh cookie");
    assert!(cookie.starts_with("refresh_token="));
    assert!(cookie.contains("HttpOnly"));

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE phone = $1")
        .bind(PHONE)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn phone_is_normalized_before_otp_verification(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool);
    save_register_code(&store, PHONE).await;

    let (status, cookie, body) = register(
        &state,
        json!({
            "phone": format!("  {PHONE}  "),
            "password": PASSWORD,
            "code": CODE
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert!(cookie.is_some());
    assert_eq!(body["user"]["phone"], PHONE);
}

#[sqlx::test]
async fn wrong_register_code_is_401_and_creates_no_user(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    save_register_code(&store, PHONE).await;

    let (status, cookie, _) = register(
        &state,
        json!({"phone": PHONE, "password": PASSWORD, "code": "000000"}),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(cookie.is_none());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "验证码错误不得创建用户");
}

#[sqlx::test]
async fn register_code_is_single_use(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool);
    save_register_code(&store, PHONE).await;

    let first = register(
        &state,
        json!({"phone": PHONE, "password": PASSWORD, "code": CODE}),
    )
    .await;
    assert_eq!(first.0, StatusCode::CREATED);

    let second = register(
        &state,
        json!({"phone": PHONE, "password": PASSWORD, "code": CODE}),
    )
    .await;
    assert_eq!(second.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn duplicate_phone_is_409(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool);
    save_register_code(&store, PHONE).await;
    let first = register(
        &state,
        json!({"phone": PHONE, "password": PASSWORD, "code": CODE}),
    )
    .await;
    assert_eq!(first.0, StatusCode::CREATED);

    // 第二次注册必须使用一枚新验证码，才能真正走到手机号唯一约束。
    save_register_code(&store, PHONE).await;
    let (status, cookie, body) = register(
        &state,
        json!({"phone": PHONE, "password": PASSWORD, "code": CODE}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(cookie.is_none());
    assert_eq!(body["detail"], "user already exists");
}

#[sqlx::test]
async fn invalid_phone_and_password_are_400(pool: PgPool) {
    let (state, _) = AppState::for_test_with_otp_store(pool);

    let invalid_phone = register(
        &state,
        json!({"phone": "12345", "password": PASSWORD, "code": CODE}),
    )
    .await;
    assert_eq!(invalid_phone.0, StatusCode::BAD_REQUEST);
    assert_eq!(invalid_phone.2["code"], "invalid_phone");
    assert_eq!(invalid_phone.2["field"], "phone");

    let invalid_password = register(
        &state,
        json!({"phone": PHONE, "password": "short", "code": CODE}),
    )
    .await;
    assert_eq!(invalid_password.0, StatusCode::BAD_REQUEST);
    assert_eq!(invalid_password.2["code"], "password_too_short");
    assert_eq!(invalid_password.2["field"], "password");
}

#[sqlx::test]
async fn email_only_payload_is_rejected(pool: PgPool) {
    let (state, _) = AppState::for_test_with_otp_store(pool);
    let (status, cookie, body) = register(
        &state,
        json!({"email": "user@example.com", "password": PASSWORD, "code": CODE}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(cookie.is_none());
    assert_eq!(body["code"], "invalid_request_body");
}

#[sqlx::test]
async fn malformed_json_uses_structured_error_response(pool: PgPool) {
    let (state, _) = AppState::for_test_with_otp_store(pool);
    let response = tsz_rust::router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/register")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["code"], "invalid_json");
}

/// 带上（或省略）反代注入的 `X-Forwarded-For` 发一次注册，返回响应状态。
async fn register_forwarded(state: &AppState, forwarded_for: Option<&str>) -> StatusCode {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/register")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(value) = forwarded_for {
        builder = builder.header("x-forwarded-for", value);
    }
    let body = json!({"phone": PHONE, "password": PASSWORD, "code": CODE}).to_string();
    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    response.status()
}

async fn stored_registration_ip(pool: &PgPool) -> Option<String> {
    sqlx::query_scalar("SELECT registration_ip FROM users WHERE phone = $1")
        .bind(PHONE)
        .fetch_one(pool)
        .await
        .expect("注册用户应已落库")
}

#[sqlx::test]
async fn records_leftmost_forwarded_for_as_registration_ip(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    save_register_code(&store, PHONE).await;

    // 反代注入的形态是 `客户端, 中间跳...`，最左一段才是真实来源。
    let status = register_forwarded(&state, Some("203.0.113.9, 10.0.0.1")).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        stored_registration_ip(&pool).await.as_deref(),
        Some("203.0.113.9"),
        "应只保留最左侧的客户端地址"
    );
}

#[sqlx::test]
async fn normalizes_ipv6_registration_ip(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    save_register_code(&store, PHONE).await;

    let status = register_forwarded(&state, Some("2001:0db8:0:0:0:0:0:1")).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        stored_registration_ip(&pool).await.as_deref(),
        Some("2001:db8::1"),
        "IPv6 地址应按标准文本形式归一化"
    );
}

#[sqlx::test]
async fn registration_succeeds_without_forwarded_for(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    save_register_code(&store, PHONE).await;

    // 反代没配 XFF（或本地直连）时，注册不能因为拿不到 IP 就失败。
    let status = register_forwarded(&state, None).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(stored_registration_ip(&pool).await, None);
}

#[sqlx::test]
async fn ignores_malformed_forwarded_for(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    save_register_code(&store, PHONE).await;

    // 畸形值不落库，避免把任意头内容当成地址存进去。
    let status = register_forwarded(&state, Some("not-an-ip")).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(stored_registration_ip(&pool).await, None);
}
