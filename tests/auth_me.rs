//! `GET /api/v1/auth/me` handler 端到端测试（真库 + `oneshot`）。
//!
//! 提取器本身的头解析边界在 [`tests/auth_extract.rs`]；这里只验 **handler 独有的**：
//! - 活跃用户带合法 token → 200，body 是**该用户**的 profile，且**不泄露敏感字段**
//! - 缺 token / 过期 token → 401
//! - token 签名有效但账号**被禁**(决策③) / **已删** → 401
//!
//! ⚠️ /me 的“禁用→401”与 login 的“密码对+禁用→403”**语义不同**：login 是认证语境（凭证对但不让进），
//! /me 是会话语境（token 有效但会话已失效）。两处状态码有别是有意的。

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Duration;
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::auth::{Realm, TokenManager};
use tsz_rust::state::AppState;
use tsz_rust::user::repository::UserRepository;
use tsz_rust::user::service::{RegisterInput, UserService};

/// 注册一个用户（密码 "password123"，只绑手机），返回 id。
async fn register_user(pool: &PgPool, phone: &str) -> Uuid {
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

/// GET /api/v1/auth/me（可选 Authorization 头），返回 (状态码, body JSON)。
async fn get_me(state: &AppState, auth: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri("/api/v1/auth/me");
    if let Some(a) = auth {
        builder = builder.header(header::AUTHORIZATION, a);
    }
    let resp = tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[sqlx::test]
async fn active_user_gets_profile(pool: PgPool) {
    let user_id = register_user(&pool, "13800138000").await;
    let state = AppState::for_test(pool);
    // 用 state 自己的 token_manager 签，才能被同一 manager parse 通过。
    let token = state.token_manager.generate(user_id, "student").unwrap();

    let (status, body) = get_me(&state, Some(&format!("Bearer {token}"))).await;

    assert_eq!(status, StatusCode::OK, "活跃用户带合法 token 应 200");
    assert_eq!(
        body["id"].as_str(),
        Some(user_id.to_string().as_str()),
        "id 应为该 token 的属主"
    );
    assert_eq!(body["phone"].as_str(), Some("13800138000"));
    assert!(body["email"].is_null(), "该用户没绑邮箱，email 应为 null");
    // 新契约：roles 是真实角色表查出的数组，last_active_role 是当前活跃角色
    assert_eq!(
        body["roles"].as_array().map(|a| a.len()),
        Some(1),
        "注册默认恰好一个角色"
    );
    assert_eq!(body["roles"][0].as_str(), Some("student"));
    assert_eq!(body["last_active_role"].as_str(), Some("student"));
    // 不泄露敏感字段
    assert!(
        !body.to_string().contains("$2b$"),
        "profile 不得含 bcrypt hash 片段"
    );
    assert!(
        body.get("password_hash").is_none(),
        "profile 不得含 password_hash 字段"
    );
}

#[sqlx::test]
async fn no_token_is_401(pool: PgPool) {
    let state = AppState::for_test(pool);
    let (status, _) = get_me(&state, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "缺 token 应 401");
}

#[sqlx::test]
async fn expired_token_is_401(pool: PgPool) {
    let user_id = register_user(&pool, "13800138000").await;
    let state = AppState::for_test(pool);
    // 相同 secret+realm、负 TTL → 出生即过期。secret/realm 对齐 AppState::for_test。
    let expired = TokenManager::new("test-secret", Realm::Web, Duration::seconds(-3600))
        .generate(user_id, "student")
        .unwrap();
    let (status, _) = get_me(&state, Some(&format!("Bearer {expired}"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "过期 token 应 401");
}

#[sqlx::test]
async fn disabled_account_is_401(pool: PgPool) {
    // token 签名有效，但账号被禁 → handler load user 后拦成 401（决策③）。
    let user_id = register_user(&pool, "13800138000").await;
    sqlx::query!(
        "UPDATE users SET status = 'disabled' WHERE id = $1",
        user_id
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::for_test(pool);
    let token = state.token_manager.generate(user_id, "student").unwrap();

    let (status, _) = get_me(&state, Some(&format!("Bearer {token}"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "禁用账号应 401");
}

#[sqlx::test]
async fn deleted_user_is_401(pool: PgPool) {
    // token 有效但 sub 在库里不存在（账号已删 / 伪造 sub）→ get_by_id NotFound → 401。
    let state = AppState::for_test(pool);
    let ghost = Uuid::now_v7();
    let token = state.token_manager.generate(ghost, "student").unwrap();
    let (status, _) = get_me(&state, Some(&format!("Bearer {token}"))).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "token 有效但账号不存在应 401"
    );
}
