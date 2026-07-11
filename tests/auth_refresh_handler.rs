//! `POST /auth/refresh` 与 `POST /auth/logout` handler 的端到端测试（真库 + `oneshot`）。
//!
//! 验 handler 层的编排与翻译：轮换出新 token 对、旧 token 单次使用即失效、失效态不可区分、
//! 禁用账号发不出 token、登出后 token 立即作废且幂等。
//! CAS 状态机本身在 `tests/session_repository.rs`、rotate/logout 的哈希接线在 `tests/session_service.rs`。

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

use tsz_rust::state::AppState;
use tsz_rust::user::repository::UserRepository;
use tsz_rust::user::service::{RegisterInput, UserService};

/// 注册一个用户（密码 "password123"），返回 id。
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

/// 通用 POST：返回 (状态码, 响应体 JSON)。
async fn post(pool: PgPool, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = tsz_rust::router(AppState::for_test(pool))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
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

/// 登录拿一枚可用的 refresh token（用户须已注册）。
async fn login_for_refresh(pool: &PgPool, phone: &str) -> String {
    let (status, body) = post(
        pool.clone(),
        "/auth/login",
        json!({ "identifier": phone, "password": "password123" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "预置登录应成功");
    body["refresh_token"]
        .as_str()
        .expect("登录应返回 refresh_token")
        .to_owned()
}

// ————————————————————— 成功 + 轮换 —————————————————————

/// 有效 refresh → 200 + OAuth 四字段齐全，且发的是一枚**新的** refresh（轮换）。
#[sqlx::test]
async fn refresh_returns_200_with_rotated_token_pair(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await;

    let (status, body) = post(
        pool.clone(),
        "/auth/refresh",
        json!({ "refresh_token": r0 }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "有效 refresh 应 200");
    assert!(
        body["access_token"].as_str().is_some_and(|s| !s.is_empty()),
        "应有非空 access_token"
    );
    let new_refresh = body["refresh_token"].as_str().expect("应有 refresh_token");
    assert!(!new_refresh.is_empty(), "应有非空 refresh_token");
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["expires_in"], 900, "for_test 的 access TTL=15min=900s");
    assert_eq!(
        body["access_token"].as_str().unwrap().split('.').count(),
        3,
        "access_token 应是三段式 JWT"
    );
    assert_ne!(new_refresh, r0, "轮换后应发一枚不同于旧的 refresh token");
}

/// 单次使用：旧 refresh 用一次成功轮换后，再用即 401（旧的一经轮换即作废）。
#[sqlx::test]
async fn old_refresh_is_rejected_after_rotation(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await;

    let (s1, _) = post(
        pool.clone(),
        "/auth/refresh",
        json!({ "refresh_token": r0.clone() }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "首次刷新应成功");

    let (s2, body) = post(pool, "/auth/refresh", json!({ "refresh_token": r0 })).await;
    assert_eq!(s2, StatusCode::UNAUTHORIZED, "已轮换的旧 token 再用应 401");
    assert_eq!(body["error"].as_str(), Some("invalid refresh token"));
}

/// 轮换链能续：用轮换出的**新** refresh 再刷，仍成功。
#[sqlx::test]
async fn rotated_new_refresh_is_usable(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await;

    let (s1, b1) = post(
        pool.clone(),
        "/auth/refresh",
        json!({ "refresh_token": r0 }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    let r1 = b1["refresh_token"].as_str().unwrap().to_owned();

    let (s2, _) = post(pool, "/auth/refresh", json!({ "refresh_token": r1 })).await;
    assert_eq!(s2, StatusCode::OK, "轮换出的新 refresh 应可继续使用");
}

// ————————————————————— 失败：失效态不可区分 —————————————————————

/// 纯垃圾串 → 401 invalid refresh token。
#[sqlx::test]
async fn garbage_refresh_is_401(pool: PgPool) {
    let (status, body) = post(
        pool,
        "/auth/refresh",
        json!({ "refresh_token": "definitely-not-a-real-token" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"].as_str(), Some("invalid refresh token"));
}

/// 已轮换的旧 token 与纯垃圾串 → 响应**逐字节一致**（不泄露 token 处于哪种失效态）。
#[sqlx::test]
async fn reused_and_garbage_are_identical_401(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await;

    // 预热：把 r0 用掉（轮换成新的），r0 变成「已轮换」失效态。
    let _ = post(
        pool.clone(),
        "/auth/refresh",
        json!({ "refresh_token": r0.clone() }),
    )
    .await;

    let (s_reused, b_reused) = post(
        pool.clone(),
        "/auth/refresh",
        json!({ "refresh_token": r0 }),
    )
    .await;
    let (s_garbage, b_garbage) = post(
        pool,
        "/auth/refresh",
        json!({ "refresh_token": "definitely-not-a-real-token" }),
    )
    .await;

    assert_eq!(s_reused, StatusCode::UNAUTHORIZED);
    assert_eq!(s_garbage, StatusCode::UNAUTHORIZED);
    assert_eq!(s_reused, s_garbage, "两种失效态状态码必须一致");
    assert_eq!(
        b_reused, b_garbage,
        "两种失效态响应体必须逐字节一致（不可区分）"
    );
}

// ————————————————————— 禁用账号 —————————————————————

/// 禁用账号（禁用前已登录拿到 token）→ refresh 得 401，且一枚新 token 都发不出。
///
/// 注：对外文案（现实现是 "user is disabled"，若按设计 Q3 统一则是 "invalid refresh token"）
/// 取决于①的决策——这里只钉「401 且发不出 token」这条安全底线，不锁死具体文案。
#[sqlx::test]
async fn disabled_user_refresh_is_401_without_tokens(pool: PgPool) {
    let user_id = register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await; // 必须禁用前登录

    sqlx::query!(
        "UPDATE users SET status = 'disabled' WHERE id = $1",
        user_id
    )
    .execute(&pool)
    .await
    .unwrap();

    let (status, body) = post(pool, "/auth/refresh", json!({ "refresh_token": r0 })).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED, "禁用账号 refresh 应 401");
    assert!(
        body["access_token"].is_null(),
        "禁用账号不应拿到 access_token"
    );
    assert!(
        body["refresh_token"].is_null(),
        "禁用账号不应拿到新 refresh_token"
    );
}

// ————————————————————— 登出 —————————————————————

/// 登出后该 refresh 立即失效：logout → 200，再 refresh → 401。
#[sqlx::test]
async fn logout_then_refresh_is_401(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await;

    let (s_logout, _) = post(
        pool.clone(),
        "/auth/logout",
        json!({ "refresh_token": r0.clone() }),
    )
    .await;
    assert_eq!(s_logout, StatusCode::NO_CONTENT, "登出应成功（204 No Content）");

    let (s_refresh, body) = post(pool, "/auth/refresh", json!({ "refresh_token": r0 })).await;
    assert_eq!(s_refresh, StatusCode::UNAUTHORIZED, "登出后再刷应 401");
    assert_eq!(body["error"].as_str(), Some("invalid refresh token"));
}

/// 登出幂等且不泄露：重复登出、以及对从不存在的 token 登出，都应 200。
#[sqlx::test]
async fn logout_is_idempotent_and_silent(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await;

    let (s1, _) = post(
        pool.clone(),
        "/auth/logout",
        json!({ "refresh_token": r0.clone() }),
    )
    .await;
    let (s2, _) = post(
        pool.clone(),
        "/auth/logout",
        json!({ "refresh_token": r0 }),
    )
    .await;
    assert_eq!(s1, StatusCode::NO_CONTENT);
    assert_eq!(s2, StatusCode::NO_CONTENT, "重复登出也应 204（幂等）");

    let (s3, _) = post(
        pool,
        "/auth/logout",
        json!({ "refresh_token": "never-existed" }),
    )
    .await;
    assert_eq!(s3, StatusCode::NO_CONTENT, "登出未知 token 也应 204，不报错");
}
