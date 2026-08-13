//! C 端账号注销 handler 安全契约：本人联系方式、固定 purpose、OTP 单次消费、
//! 并发防重放、事务级 cascade、全 session 失效与 refresh cookie 清理。

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::session::{repository::RefreshTokenRepository, service::SessionService};
use tsz_rust::{otp::model::Purpose, router, state::AppState};
use uuid::Uuid;

async fn seed_user(pool: &PgPool, phone: Option<&str>, email: Option<&str>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, phone, email, password_hash, display_name, last_active_role) \
         VALUES ($1, $2, $3, 'hash', 'delete-me', 'student')",
    )
    .bind(id)
    .bind(phone)
    .bind(email)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, 'student')")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, 'teacher')")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO student_profiles (user_id) VALUES ($1)")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO teacher_profiles (user_id) VALUES ($1)")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .bind(format!("hash-{id}"))
    .bind(Utc::now() + Duration::days(30))
    .execute(pool)
    .await
    .unwrap();
    id
}

fn bearer(state: &AppState, user_id: Uuid) -> String {
    format!(
        "Bearer {}",
        state.token_manager.generate(user_id, "student").unwrap()
    )
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> axum::response::Response {
    router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, token)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, "refresh_token=secret-cookie")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn problem(response: axum::response::Response) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[sqlx::test]
async fn request_uses_current_users_contact_and_fixed_deletion_purpose(pool: PgPool) {
    let user_id = seed_user(&pool, Some("13800138000"), Some("owner@example.com")).await;
    let (state, store) = AppState::for_test_with_otp_store(pool);
    let response = call(
        &state,
        "POST",
        "/api/v1/auth/account/deletion-code",
        &bearer(&state, user_id),
        json!({"channel":"email", "purpose":"login", "target":"attacker@example.com"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(
        store
            .code_exists("owner@example.com", Purpose::AccountDeletion)
            .await
            .unwrap()
    );
    assert!(
        !store
            .code_exists("attacker@example.com", Purpose::AccountDeletion)
            .await
            .unwrap()
    );
    assert!(
        !store
            .code_exists("owner@example.com", Purpose::Login)
            .await
            .unwrap()
    );
}

#[sqlx::test]
async fn unavailable_channel_is_stable_problem_and_sends_nothing(pool: PgPool) {
    let user_id = seed_user(&pool, Some("13800138001"), None).await;
    let (state, store) = AppState::for_test_with_otp_store(pool);
    let response = call(
        &state,
        "POST",
        "/api/v1/auth/account/deletion-code",
        &bearer(&state, user_id),
        json!({"channel":"email"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert_eq!(
        problem(response).await["code"],
        "account_deletion_channel_unavailable"
    );
    assert!(
        !store
            .code_exists("13800138001", Purpose::AccountDeletion)
            .await
            .unwrap()
    );
}

#[sqlx::test]
async fn wrong_code_is_undifferentiated_and_preserves_account(pool: PgPool) {
    let user_id = seed_user(&pool, Some("13800138002"), None).await;
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    store
        .save_code(
            "13800138002",
            Purpose::AccountDeletion,
            "123456",
            std::time::Duration::from_secs(300),
        )
        .await
        .unwrap();
    let response = call(
        &state,
        "DELETE",
        "/api/v1/auth/account",
        &bearer(&state, user_id),
        json!({"channel":"phone", "code":"654321"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert_eq!(
        problem(response).await["code"],
        "invalid_account_deletion_code"
    );
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(exists);
}

#[sqlx::test]
async fn success_cascades_and_clears_cookie_and_replay_fails(pool: PgPool) {
    let user_id = seed_user(&pool, Some("13800138003"), None).await;
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let issued_refresh = SessionService::new(
        RefreshTokenRepository::new(pool.clone()),
        Duration::days(30),
    )
    .issue(user_id)
    .await
    .unwrap();
    store
        .save_code(
            "13800138003",
            Purpose::AccountDeletion,
            "123456",
            std::time::Duration::from_secs(300),
        )
        .await
        .unwrap();
    let token = bearer(&state, user_id);
    let response = call(
        &state,
        "DELETE",
        "/api/v1/auth/account",
        &token,
        json!({"channel":"phone", "code":"123456"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.starts_with("refresh_token="));
    assert!(cookie.contains("Max-Age=0"));
    assert!(cookie.contains("Path=/api/v1/auth"));

    let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
          (SELECT COUNT(*) FROM users WHERE id = $1), \
          (SELECT COUNT(*) FROM user_roles WHERE user_id = $1), \
          (SELECT COUNT(*) FROM student_profiles WHERE user_id = $1), \
          (SELECT COUNT(*) FROM teacher_profiles WHERE user_id = $1), \
          (SELECT COUNT(*) FROM refresh_tokens WHERE user_id = $1)",
    )
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    for (table, count) in [
        ("users", counts.0),
        ("user_roles", counts.1),
        ("student_profiles", counts.2),
        ("teacher_profiles", counts.3),
        ("refresh_tokens", counts.4),
    ] {
        assert_eq!(count, 0, "{table} 应被删除/cascade 清理");
    }

    let replay = call(
        &state,
        "DELETE",
        "/api/v1/auth/account",
        &token,
        json!({"channel":"phone", "code":"123456"}),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);

    let me = router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/me")
                .header(header::AUTHORIZATION, &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);

    let refresh = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/refresh")
                .header(
                    header::COOKIE,
                    format!("refresh_token={}", issued_refresh.plaintext),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn concurrent_confirmation_has_exactly_one_winner(pool: PgPool) {
    let user_id = seed_user(&pool, Some("13800138004"), None).await;
    let (state, store) = AppState::for_test_with_otp_store(pool);
    store
        .save_code(
            "13800138004",
            Purpose::AccountDeletion,
            "123456",
            std::time::Duration::from_secs(300),
        )
        .await
        .unwrap();
    let token = bearer(&state, user_id);
    let body = json!({"channel":"phone", "code":"123456"});
    let (a, b) = tokio::join!(
        call(
            &state,
            "DELETE",
            "/api/v1/auth/account",
            &token,
            body.clone()
        ),
        call(&state, "DELETE", "/api/v1/auth/account", &token, body),
    );
    let mut statuses = [a.status(), b.status()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::NO_CONTENT, StatusCode::UNAUTHORIZED]);
}
