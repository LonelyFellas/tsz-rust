//! `POST /api/v1/auth/login-otp`（手机验证码登录）端到端测试。
//!
//! 编排契约（本文件钉死）：
//! - 验码通过 + 活跃用户 → **200** + OAuth 双 token，且**不泄露 hash**
//! - 错码 → **401**；**未知标识（无账号）→ 同一个 401**（不泄露是否注册、**不自动注册**）
//! - 验码对但账号禁用 → **403**（proof-of-code 后才透传禁用态，与密码登录一致）
//! - 从没发过码 / purpose 不匹配 → **401**（统一 InvalidCode）
//! - 请求体里显式塞 `purpose` → 被无视：handler 写死 `Purpose::Login`，
//!   持有 password_reset 码也换不来登录会话（purpose 隔离不可被客户端绕过）
//!
//! 拿码方式：`for_test_with_otp_store` 返回与 handler 共享 keyspace 的 `OtpStore`，
//! 直接 `save_code` 注入**已知** code（`OtpSender::Mock` 不回传码，走注入最干净）。
//!
//! ⚠️ 硬依赖本地活 Redis；跨用例隔离靠每次 `for_test_with_otp_store` 的唯一 key 前缀。

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use tower::ServiceExt;

use tsz_rust::otp::model::Purpose;
use tsz_rust::state::AppState;
use tsz_rust::user::repository::UserRepository;
use tsz_rust::user::service::{RegisterInput, UserService};

const CODE: &str = "123456";
fn ttl() -> Duration {
    Duration::from_secs(300)
}

/// 注册一个手机用户（密码登录用不到，但建号才能走到发 token）。
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

/// POST /api/v1/auth/login-otp，返回 (状态码, Set-Cookie 头, body JSON)。
async fn login_otp(state: &AppState, body: Value) -> (StatusCode, Option<String>, Value) {
    let resp = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login-otp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        set_cookie,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[sqlx::test]
async fn valid_code_active_user_gets_tokens(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    register_user(&pool, "13800138000").await;
    store
        .save_code("13800138000", Purpose::Login, CODE, ttl())
        .await
        .unwrap();

    let (status, set_cookie, body) =
        login_otp(&state, json!({"identifier": "13800138000", "code": CODE})).await;

    assert_eq!(status, StatusCode::OK, "验码通过 + 活跃用户应 200");
    // 与密码登录同形状（契约 T1，共用 build_login_response）：token 字段平铺顶层、
    // 用户对象嵌在 user 下，refresh 走 httpOnly cookie 不进 body。
    // 形状细节（active_role 位置 / 可选字段省略 / expires_at 真值）由 auth_login_handler
    // 全量钉死，这里只验共用点没漂 + OTP 独有的 cookie 路径。
    assert!(
        body["access_token"].as_str().is_some_and(|s| !s.is_empty()),
        "顶层应有非空 access_token"
    );
    assert!(
        body["user"]["active_role"].as_str().is_some(),
        "user.active_role 应存在（与密码登录同形状）"
    );
    assert!(
        body.get("refresh_token").is_none() && body.get("token").is_none(),
        "body 不得有 refresh_token 键或嵌套 token 对象"
    );
    // OTP 登录的 cookie 下发是独立代码路径（不与密码登录共用），必须单独验
    let cookie = set_cookie.as_deref().expect("OTP 登录也应下发 Set-Cookie");
    assert!(
        cookie.starts_with("refresh_token=") && cookie.contains("HttpOnly"),
        "refresh cookie 应存在且 HttpOnly：{cookie}"
    );
    assert!(
        !body.to_string().contains("$2b$"),
        "响应不得含 bcrypt hash 片段"
    );
}

#[sqlx::test]
async fn wrong_code_is_401(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    register_user(&pool, "13800138000").await;
    store
        .save_code("13800138000", Purpose::Login, CODE, ttl())
        .await
        .unwrap();

    let (status, _, _) = login_otp(
        &state,
        json!({"identifier": "13800138000", "code": "000000"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "错码应 401");
}

#[sqlx::test]
async fn unknown_identifier_is_401(pool: PgPool) {
    // 未注册号也能收到码（RequestLoginCode 不校验注册）→ 验码可过，但查无账号 → 401，
    // 且与错码**同一个** 401，不泄露注册状态；这里连账号都不建。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    store
        .save_code("13900139000", Purpose::Login, CODE, ttl())
        .await
        .unwrap();

    let (status, _, _) =
        login_otp(&state, json!({"identifier": "13900139000", "code": CODE})).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "验码对但无账号应 401（不自动注册）"
    );
}

#[sqlx::test]
async fn disabled_account_is_403(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let uid = register_user(&pool, "13800138000").await;
    sqlx::query!("UPDATE users SET status = 'disabled' WHERE id = $1", uid)
        .execute(&pool)
        .await
        .unwrap();
    store
        .save_code("13800138000", Purpose::Login, CODE, ttl())
        .await
        .unwrap();

    let (status, _, _) =
        login_otp(&state, json!({"identifier": "13800138000", "code": CODE})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "验码对但账号禁用应 403");
}

#[sqlx::test]
async fn no_code_sent_is_401(pool: PgPool) {
    // 从没发过码 → verify 得 InvalidCode → 401。
    let (state, _store) = AppState::for_test_with_otp_store(pool.clone());
    register_user(&pool, "13800138000").await;

    let (status, _, _) =
        login_otp(&state, json!({"identifier": "13800138000", "code": CODE})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "没发过码应 401");
}

#[sqlx::test]
async fn wrong_purpose_code_is_401(pool: PgPool) {
    // 码是发给 password_reset 的，拿来登录 → Redis key（含 purpose）不匹配 → 401。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    register_user(&pool, "13800138000").await;
    store
        .save_code("13800138000", Purpose::PasswordReset, CODE, ttl())
        .await
        .unwrap();

    let (status, _, _) =
        login_otp(&state, json!({"identifier": "13800138000", "code": CODE})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "purpose 不匹配应 401");
}

#[sqlx::test]
async fn explicit_reset_purpose_in_body_is_ignored_401(pool: PgPool) {
    // 对抗用例：客户端持有一枚**有效的** password_reset 码，并在请求体里显式塞
    // `"purpose": "password_reset"`，妄图拿重置码换一个登录会话。
    // handler 写死 Purpose::Login、无视请求体 purpose → 按 Login 查 key → 对不上 → 401。
    // 若哪天有人把 purpose 改回“从请求体取”，本用例会立刻变红。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    register_user(&pool, "13800138000").await;
    store
        .save_code("13800138000", Purpose::PasswordReset, CODE, ttl())
        .await
        .unwrap();

    let (status, _, _) = login_otp(
        &state,
        json!({"identifier": "13800138000", "code": CODE, "purpose": "password_reset"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "请求体里的 purpose 应被无视，重置码换不来登录会话"
    );
}

#[sqlx::test]
async fn email_login_otp_is_case_insensitive(pool: PgPool) {
    // 回归锁：OTP 登录对邮箱大小写不敏感（与密码登录一致）。
    // 发码/存码走归一化后的小写邮箱；登录用大写邮箱。handler 先 normalize_identifier
    // 再 verify + 查账号 → 命中同一 key、同一账号 → 200。
    // 若哪端漏了归一化（verify 用原始串 / send 不归一化），大写登录会查不到码 → 401，本用例立刻变红。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    // 注册邮箱用户（register 内部把邮箱归一化为小写入库）
    UserService::new(UserRepository::new(pool.clone()))
        .register(RegisterInput {
            phone: None,
            email: Some("User@X.com".to_owned()),
            password: "password123".to_owned(),
        })
        .await
        .expect("注册应成功");
    // 码存在归一化后的小写邮箱下（send_otp 现在就是这么存的）
    store
        .save_code("user@x.com", Purpose::Login, CODE, ttl())
        .await
        .unwrap();

    // 登录用大写邮箱 + 正确码 → 应 200
    let (status, _, body) =
        login_otp(&state, json!({"identifier": "User@X.com", "code": CODE})).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "大写邮箱 + 正确码应 200（大小写不敏感）"
    );
    assert!(
        body["access_token"].as_str().is_some_and(|s| !s.is_empty()),
        "应发出非空 access_token"
    );
}
