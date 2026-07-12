//! `POST /auth/login` handler 的端到端测试（真库 + `oneshot`）。
//!
//! 验 handler 层独有的翻译：状态码映射、响应形状、**两种失败不可区分**、不泄露 hash。
//! 凭证校验的细节在 `tests/user_authenticate.rs`（service 层）。

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

use tsz_rust::state::AppState;
use tsz_rust::user::repository::UserRepository;
use tsz_rust::user::service::{RegisterInput, UserService};

/// 先注册一个用户（密码 "password123"），返回 id。
async fn register_user(pool: &PgPool, phone: &str) -> uuid::Uuid {
    UserService::new(UserRepository::new(pool.clone()))
        .register(RegisterInput {
            phone: Some(phone.to_owned()),
            email: None,
            password: "password123".to_owned(),
        })
        .await
        .expect("注册应成功")
        .id
}

/// POST /auth/login，返回 (状态码, 响应体 JSON)。
async fn login(pool: PgPool, body: Value) -> (StatusCode, Value) {
    let resp = tsz_rust::router(AppState::for_test(pool))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

// ————————————————————— 成功 —————————————————————

/// 正确凭证 → 200 + 四字段齐全、不泄露 hash，且 refresh 已落库。
#[sqlx::test]
async fn login_returns_200_with_tokens(pool: PgPool) {
    let user_id = register_user(&pool, "13800138000").await;

    let (status, body) = login(
        pool.clone(),
        json!({ "identifier": "13800138000", "password": "password123" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "正确凭证应 200");

    // OAuth 形状四字段
    assert!(
        body["access_token"].as_str().is_some_and(|s| !s.is_empty()),
        "应有非空 access_token"
    );
    assert!(
        body["refresh_token"].as_str().is_some_and(|s| !s.is_empty()),
        "应有非空 refresh_token"
    );
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["expires_in"], 900, "for_test 的 access TTL=15min=900s");

    // access_token 看着像 JWT（三段）
    assert_eq!(
        body["access_token"].as_str().unwrap().split('.').count(),
        3,
        "access_token 应是三段式 JWT"
    );

    // 绝不泄露 hash
    assert!(!body.to_string().contains("$2b$"), "响应不得含 bcrypt hash 片段");

    // refresh 确实落库了（该用户名下恰好一行）
    let count = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM refresh_tokens WHERE user_id = $1",
        user_id
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, Some(1), "登录应发一枚 refresh token 并落库");
}

// ————————————————————— 失败：两种都 401 且一致 —————————————————————

/// 密码错 → 401 invalid credentials。
#[sqlx::test]
async fn login_wrong_password_is_401(pool: PgPool) {
    register_user(&pool, "13800138000").await;

    let (status, body) = login(
        pool,
        json!({ "identifier": "13800138000", "password": "wrong" }),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"].as_str(), Some("invalid credentials"));
}

/// 未知用户 → 401 invalid credentials —— 和密码错**逐字节一致**（不可区分）。
#[sqlx::test]
async fn login_unknown_user_is_identical_401(pool: PgPool) {
    register_user(&pool, "13800138000").await;

    // 密码错（用户存在）
    let (s_wrong, b_wrong) = login(
        pool.clone(),
        json!({ "identifier": "13800138000", "password": "wrong" }),
    )
    .await;
    // 用户不存在
    let (s_unknown, b_unknown) = login(
        pool,
        json!({ "identifier": "19999999999", "password": "password123" }),
    )
    .await;

    assert_eq!(s_wrong, StatusCode::UNAUTHORIZED);
    assert_eq!(s_unknown, StatusCode::UNAUTHORIZED);
    assert_eq!(s_wrong, s_unknown, "两种失败状态码必须一致");
    assert_eq!(b_wrong, b_unknown, "两种失败响应体必须一致（不可区分）");
}

// ————————————————————— 禁用账号 —————————————————————

/// 密码正确但账号被禁 → 403。
#[sqlx::test]
async fn login_disabled_account_is_403(pool: PgPool) {
    let user_id = register_user(&pool, "13800138000").await;
    sqlx::query!("UPDATE users SET status = 'disabled' WHERE id = $1", user_id)
        .execute(&pool)
        .await
        .unwrap();

    let (status, _) = login(
        pool,
        json!({ "identifier": "13800138000", "password": "password123" }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "密码对+账号禁用应 403");
}
