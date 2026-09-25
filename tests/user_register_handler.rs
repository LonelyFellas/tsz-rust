//! 手机号与邮箱验证码注册的真实 PG/Redis 集成测试。

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
async fn email_registration_normalizes_and_creates_complete_session(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    save_register_code(&store, "student@example.com").await;
    let (status, cookie, body) = register(
        &state,
        json!({"email": "  Student@EXAMPLE.com  ", "password": PASSWORD, "code": CODE}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(cookie.unwrap().contains("HttpOnly"));
    assert_eq!(body["user"]["email"], "student@example.com");
    assert!(body["user"]["phone"].is_null());
    assert_eq!(body["user"]["roles"], json!(["student"]));
    let id = uuid::Uuid::parse_str(body["user"]["id"].as_str().unwrap()).unwrap();
    let row: (Option<String>, String) =
        sqlx::query_as("SELECT phone, password_hash FROM users WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(row.0.is_none());
    assert!(bcrypt::verify(PASSWORD.to_uppercase(), &row.1).unwrap());
    let roles: i64 = sqlx::query_scalar("SELECT count(*) FROM user_roles WHERE user_id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let sessions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM refresh_tokens WHERE user_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((roles, sessions), (1, 1));

    save_register_code(&store, "student@example.com").await;
    let duplicate = register(
        &state,
        json!({"email": "STUDENT@example.com", "password": PASSWORD, "code": CODE}),
    )
    .await;
    assert_eq!(duplicate.0, StatusCode::CONFLICT);
    assert!(duplicate.1.is_none());
    assert_eq!(duplicate.2["field"], "email");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn invalid_contacts_do_not_consume_registration_code(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    save_register_code(&store, "student@example.com").await;
    for contact in [
        json!({}),
        json!({"email":" "}),
        json!({"email":"invalid"}),
        json!({"phone":PHONE,"email":"student@example.com"}),
    ] {
        let mut body = contact;
        body["password"] = json!(PASSWORD);
        body["code"] = json!(CODE);
        let result = register(&state, body).await;
        assert_eq!(result.0, StatusCode::BAD_REQUEST, "{}", result.2);
        assert!(result.1.is_none());
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let result = register(
        &state,
        json!({"email":"student@example.com","password":PASSWORD,"code":CODE}),
    )
    .await;
    assert_eq!(result.0, StatusCode::CREATED);
}

#[sqlx::test]
async fn email_registration_rejects_wrong_purpose_target_and_replay(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    store
        .save_code("student@example.com", Purpose::Login, CODE, ttl())
        .await
        .unwrap();
    save_register_code(&store, "other@example.com").await;
    let body = json!({"email":"student@example.com","password":PASSWORD,"code":CODE});
    assert_eq!(
        register(&state, body.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    save_register_code(&store, "student@example.com").await;
    assert_eq!(register(&state, body.clone()).await.0, StatusCode::CREATED);
    assert_eq!(register(&state, body).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn registration_password_policy_is_checked_before_consuming_code(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool);
    save_register_code(&store, PHONE).await;
    for password in [
        "abcdefghi1",
        "abcdefghijklmnopqrstu1",
        "abcdefghijk",
        "12345678901",
        "abcdefghi1!",
        "密码abcdefghi1",
    ] {
        let result = register(
            &state,
            json!({"phone":PHONE,"password":password,"code":CODE}),
        )
        .await;
        assert_eq!(
            result.0,
            StatusCode::BAD_REQUEST,
            "{password}: {}",
            result.2
        );
        assert_eq!(result.2["field"], "password");
    }
    assert_eq!(
        register(
            &state,
            json!({"phone":PHONE,"password":PASSWORD,"code":CODE})
        )
        .await
        .0,
        StatusCode::CREATED
    );
}

#[sqlx::test]
async fn concurrent_email_registration_creates_only_one_account(pool: PgPool) {
    let (first, first_store) = AppState::for_test_with_otp_store(pool.clone());
    let (second, second_store) = AppState::for_test_with_otp_store(pool.clone());
    save_register_code(&first_store, "student@example.com").await;
    save_register_code(&second_store, "student@example.com").await;
    let (a, b) = tokio::join!(
        register(
            &first,
            json!({"email":"STUDENT@example.com","password":PASSWORD,"code":CODE})
        ),
        register(
            &second,
            json!({"email":"student@example.com","password":PASSWORD,"code":CODE})
        ),
    );
    let mut statuses = [a.0.as_u16(), b.0.as_u16()];
    statuses.sort();
    assert_eq!(statuses, [201, 409]);
    for table in ["users", "user_roles", "refresh_tokens"] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1, "{table}");
    }
}

#[sqlx::test]
async fn email_session_failure_rolls_back_user_and_role(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    sqlx::raw_sql("CREATE FUNCTION reject_test_session() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test session unavailable'; END $$; CREATE TRIGGER reject_test_session BEFORE INSERT ON refresh_tokens FOR EACH ROW EXECUTE FUNCTION reject_test_session();")
        .execute(&pool).await.unwrap();
    save_register_code(&store, "student@example.com").await;
    let result = register(
        &state,
        json!({"email":"student@example.com","password":PASSWORD,"code":CODE}),
    )
    .await;
    assert!(result.0.is_server_error());
    assert!(result.1.is_none());
    for table in ["users", "user_roles", "refresh_tokens"] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0, "{table}");
    }
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
