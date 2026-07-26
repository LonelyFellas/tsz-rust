//! `POST /api/v1/admin/auth/login`（admin 2FA 登录：手机号 + 密码 + 验证码）端到端测试。
//!
//! 编排契约（admin-design.md §6/§10，本文件钉死；**只测对外行为，不锁内部分层**——
//! 编排放 service 还是 handler 都能转绿，但下面五步顺序与响应形状不可违背）：
//!
//! ```text
//! ① 锁定先于一切：locked_until 在未来 ⇒ 423（密码/码全对也拒），且不累计、不烧码
//! ② 密码错 ⇒ register_failed_login 累计 ⇒ 401；码不被消费（码校验在密码之后）
//! ③ disabled ⇒ 403（在验证码之前：禁用账号不烧码）
//! ④ 码错 ⇒ 同样累计 ⇒ 401，且与②、查无此号**逐字节一致**（防密码爆破 oracle）
//!    码的 purpose 是 AdminLogin——web 端 login 码换不来 admin 会话（keyspace 隔离）
//! ⑤ 成功 ⇒ 清零失败计数 ⇒ 200 + access token + admin refresh cookie
//! 基础设施（Redis）故障 ⇒ 503 fail-close，不累计（不是用户的错）
//! ```
//!
//! 拿码方式与 login-otp 测试同款：`for_test_with_otp_store` 注入已知 code。
//! ⚠️ 硬依赖本地活 Redis；跨用例隔离靠每次唯一 key 前缀。

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

use tsz_rust::admin::{AdminRepository, AdminService};
use tsz_rust::otp::model::Purpose;
use tsz_rust::otp::sender::OtpSender;
use tsz_rust::otp::service::OtpService;
use tsz_rust::otp::store::OtpStore;
use tsz_rust::state::AppState;

const PHONE: &str = "13800138000";
const PASSWORD: &str = "password123";
const CODE: &str = "123456";
const WRONG_CODE: &str = "000000";
/// 与 `for_test_with_otp_store` 的 max_attempts 镜像（码注入后测试内最多验 1-2 次，远不触锁死）
const MAX_ATTEMPTS: u8 = 5;

fn ttl() -> Duration {
    Duration::from_secs(300)
}

/// 建一个可登录的 active admin（走 seed：密码过策略、must_change=false）。
async fn create_admin(pool: &PgPool, phone: &str) -> uuid::Uuid {
    let outcome = AdminService::for_seed(AdminRepository::new(pool.clone()))
        .seed_super_admin(phone, PASSWORD, "测试超管")
        .await
        .expect("seed 应成功");
    match outcome {
        tsz_rust::admin::SeedOutcome::Created(admin) => admin.id,
        tsz_rust::admin::SeedOutcome::Unchanged(admin) => admin.id,
    }
}

/// POST /api/v1/admin/auth/login，返回 (状态码, Set-Cookie, 原始 body 文本)。
/// body 返回**原始文本**而非 JSON——逐字节一致断言必须比对原文。
async fn admin_login(state: &AppState, body: Value) -> (StatusCode, Option<String>, String) {
    let resp = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/auth/login")
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
        String::from_utf8(bytes.to_vec()).unwrap(),
    )
}

fn login_body(phone: &str, password: &str, code: &str) -> Value {
    json!({ "phone": phone, "password": password, "code": code })
}

async fn failed_count(pool: &PgPool, phone: &str) -> i32 {
    sqlx::query_scalar!(
        "SELECT failed_login_count FROM admins WHERE phone = $1",
        phone
    )
    .fetch_one(pool)
    .await
    .expect("admin 行应存在")
}

async fn locked_until(pool: &PgPool, phone: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    sqlx::query_scalar!("SELECT locked_until FROM admins WHERE phone = $1", phone)
        .fetch_one(pool)
        .await
        .expect("admin 行应存在")
}

// ============================== ⑤ 成功路径 ==============================

#[sqlx::test]
async fn valid_factors_login_succeeds_and_clears_counter(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let id = create_admin(&pool, PHONE).await;
    // 预埋残留失败计数：成功登录必须清零（⑤），否则下次输错 1 次就可能直接锁
    sqlx::query!("UPDATE admins SET failed_login_count = 3 WHERE id = $1", id)
        .execute(&pool)
        .await
        .unwrap();
    store
        .save_code(PHONE, Purpose::AdminLogin, CODE, ttl())
        .await
        .unwrap();

    let (status, set_cookie, body) = admin_login(&state, login_body(PHONE, PASSWORD, CODE)).await;

    assert_eq!(status, StatusCode::OK, "三因子全对应 200：{body}");
    let json: Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["access_token"].as_str().is_some_and(|s| !s.is_empty()),
        "顶层应有非空 access_token"
    );
    assert_eq!(json["admin_profile"]["phone"], PHONE, "profile 应是本人");
    assert!(
        json["refresh_token_expires_at"].as_i64().is_some(),
        "应有 refresh_token_expires_at（Unix 秒）"
    );
    assert!(
        json.get("refresh_token").is_none(),
        "refresh token 只走 cookie，不得进 body"
    );
    assert!(!body.contains("$2b$"), "响应不得含 bcrypt hash 片段");
    // Q10 处置（2026-07-26）：permissions 死数据字段已整删（连同前端菜单 key 过滤），
    // login 响应顶层与 admin_profile 概要内都不得回潮。
    assert!(
        json.get("permissions").is_none() && json["admin_profile"].get("permissions").is_none(),
        "permissions 已随 Q10 处置删除，不得出现在 login 响应任何层级：{body}"
    );

    let cookie = set_cookie.as_deref().expect("应下发 admin refresh cookie");
    assert!(
        cookie.starts_with("admin_refresh_token=") && cookie.contains("HttpOnly"),
        "cookie 应为 admin_refresh_token 且 HttpOnly：{cookie}"
    );
    assert!(
        cookie.contains("Path=/api/v1/admin/auth"),
        "cookie Path 应钉在 admin auth 挂载点：{cookie}"
    );

    assert_eq!(
        failed_count(&pool, PHONE).await,
        0,
        "成功登录应清零失败计数（⑤）"
    );
}

// ==================== ②④ 三种失败逐字节一致 + 各自副作用 ====================

#[sqlx::test]
async fn three_failure_modes_are_byte_identical_401(pool: PgPool) {
    // 查无此号 / 密码错 / 码错——status 与 body **逐字节一致**，
    // 否则验证码层就成了「密码对不对」的确认器（§6④）。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    create_admin(&pool, PHONE).await;
    store
        .save_code(PHONE, Purpose::AdminLogin, CODE, ttl())
        .await
        .unwrap();

    // 未注册手机号（连码都没有）
    let (s1, _, b1) = admin_login(&state, login_body("13999990000", PASSWORD, CODE)).await;
    // 密码错（码是对的、在库里）
    let (s2, _, b2) = admin_login(&state, login_body(PHONE, "wrong-password", CODE)).await;
    // 密码对、码错
    let (s3, _, b3) = admin_login(&state, login_body(PHONE, PASSWORD, WRONG_CODE)).await;

    assert_eq!(s1, StatusCode::UNAUTHORIZED, "查无此号应 401");
    assert_eq!(s2, StatusCode::UNAUTHORIZED, "密码错应 401");
    assert_eq!(
        s3,
        StatusCode::UNAUTHORIZED,
        "码错应 401（不是 500，也不是别的文案）"
    );
    assert_eq!(b1, b2, "查无此号与密码错的 body 应逐字节一致");
    assert_eq!(b2, b3, "密码错与码错的 body 应逐字节一致");
    assert_eq!(
        serde_json::from_str::<Value>(&b1).unwrap(),
        json!({ "error": "invalid credentials" }),
        "统一文案为契约文案"
    );
}

#[sqlx::test]
async fn wrong_password_accumulates_and_does_not_burn_code(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    create_admin(&pool, PHONE).await;
    store
        .save_code(PHONE, Purpose::AdminLogin, CODE, ttl())
        .await
        .unwrap();

    let (status, _, _) = admin_login(&state, login_body(PHONE, "wrong-password", CODE)).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        failed_count(&pool, PHONE).await,
        1,
        "密码错应累计失败计数（②）——锁定机制的主路径就是挡撞库"
    );
    // 码校验在密码之后：密码没过，码必须原封不动（用户改对密码后无需重新发码）
    assert!(
        store
            .verify_code(PHONE, Purpose::AdminLogin, CODE, MAX_ATTEMPTS)
            .await
            .unwrap(),
        "密码错不应烧码：原码应仍可消费"
    );
}

#[sqlx::test]
async fn wrong_code_accumulates_lockout(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    create_admin(&pool, PHONE).await;
    store
        .save_code(PHONE, Purpose::AdminLogin, CODE, ttl())
        .await
        .unwrap();

    let (status, _, _) = admin_login(&state, login_body(PHONE, PASSWORD, WRONG_CODE)).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        failed_count(&pool, PHONE).await,
        1,
        "码错也应累计锁定（④）：密码对+码错=拿到密码没拿到手机，更可疑"
    );
}

#[sqlx::test]
async fn web_login_code_cannot_cross_into_admin_realm(pool: PgPool) {
    // 对抗用例：攻击者通过**公开的** web /otp/send 给自己发一枚 purpose=login 的码，
    // 拿它过 admin 2FA。若 admin 侧误用 Purpose::Login，keyspace 撞车、此码即通行证。
    // 正解：admin 用 Purpose::AdminLogin，key 不同 → 验不过 → 401。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    create_admin(&pool, PHONE).await;
    store
        .save_code(PHONE, Purpose::Login, CODE, ttl())
        .await
        .unwrap();

    let (status, _, _) = admin_login(&state, login_body(PHONE, PASSWORD, CODE)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "web login 码不得打穿 admin 2FA（purpose keyspace 隔离）"
    );
}

// ============================== ③ disabled ==============================

#[sqlx::test]
async fn disabled_admin_is_403_and_code_not_burned(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let id = create_admin(&pool, PHONE).await;
    sqlx::query!("UPDATE admins SET status = 'disabled' WHERE id = $1", id)
        .execute(&pool)
        .await
        .unwrap();
    store
        .save_code(PHONE, Purpose::AdminLogin, CODE, ttl())
        .await
        .unwrap();

    let (status, _, _) = admin_login(&state, login_body(PHONE, PASSWORD, CODE)).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "禁用账号密码对也应 403");
    // disabled 检查在验证码**之前**：禁用账号不烧码（③）
    assert!(
        store
            .verify_code(PHONE, Purpose::AdminLogin, CODE, MAX_ATTEMPTS)
            .await
            .unwrap(),
        "禁用账号的登录尝试不应消费验证码"
    );
    assert_eq!(
        failed_count(&pool, PHONE).await,
        0,
        "disabled 不是失败因子，不累计"
    );
}

// ============================== ① 锁定 ==============================

#[sqlx::test]
async fn locked_admin_is_423_even_with_all_factors_valid(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let id = create_admin(&pool, PHONE).await;
    sqlx::query!(
        "UPDATE admins SET locked_until = NOW() + interval '10 minutes' WHERE id = $1",
        id
    )
    .execute(&pool)
    .await
    .unwrap();
    let deadline_before = locked_until(&pool, PHONE).await;
    store
        .save_code(PHONE, Purpose::AdminLogin, CODE, ttl())
        .await
        .unwrap();

    let (status, _, body) = admin_login(&state, login_body(PHONE, PASSWORD, CODE)).await;

    assert_eq!(
        status,
        StatusCode::LOCKED,
        "锁定中三因子全对也应 423，不是 401/403"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({ "error": "account temporarily locked due to too many failed login attempts" }),
        "423 文案是契约（§10），前端据此提示"
    );
    // 锁定先于一切因子：不烧码、不累计、不续锁（①）
    assert!(
        store
            .verify_code(PHONE, Purpose::AdminLogin, CODE, MAX_ATTEMPTS)
            .await
            .unwrap(),
        "锁定中的尝试不应消费验证码"
    );
    assert_eq!(failed_count(&pool, PHONE).await, 0, "锁定中不累计失败");
    assert_eq!(
        locked_until(&pool, PHONE).await,
        deadline_before,
        "锁定截止不得被尝试续期"
    );
}

#[sqlx::test]
async fn locked_admin_wrong_password_is_still_423(pool: PgPool) {
    // 「密码对错皆然」：锁定中若密码错返 401、密码对返 423，
    // 攻击者就能拿锁定窗口当免费的密码校验器——必须一律 423。
    let (state, _store) = AppState::for_test_with_otp_store(pool.clone());
    let id = create_admin(&pool, PHONE).await;
    sqlx::query!(
        "UPDATE admins SET locked_until = NOW() + interval '10 minutes' WHERE id = $1",
        id
    )
    .execute(&pool)
    .await
    .unwrap();

    let (status, _, _) = admin_login(&state, login_body(PHONE, "wrong-password", WRONG_CODE)).await;
    assert_eq!(status, StatusCode::LOCKED, "锁定中密码对错皆 423");
    assert_eq!(failed_count(&pool, PHONE).await, 0, "锁定中不累计失败");
}

#[sqlx::test]
async fn expired_lock_auto_unlocks(pool: PgPool) {
    // 过去的 locked_until 不是锁（自动解锁，无 cron）——设计 §3。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let id = create_admin(&pool, PHONE).await;
    sqlx::query!(
        "UPDATE admins SET locked_until = NOW() - interval '1 minute' WHERE id = $1",
        id
    )
    .execute(&pool)
    .await
    .unwrap();
    store
        .save_code(PHONE, Purpose::AdminLogin, CODE, ttl())
        .await
        .unwrap();

    let (status, _, body) = admin_login(&state, login_body(PHONE, PASSWORD, CODE)).await;
    assert_eq!(status, StatusCode::OK, "锁已过期应可正常登录：{body}");
}

#[sqlx::test]
async fn fifth_wrong_code_trips_the_lock(pool: PgPool) {
    // 阈值镜像 service 层常量（threshold=5）：预埋 4 次失败，第 5 次码错触锁。
    // 本次响应仍是 401（与前 4 次不可区分；423 从下一次请求开始）——
    // 若实现选择「触锁那次即返 423」，改这条断言并同步 service 注释。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let id = create_admin(&pool, PHONE).await;
    sqlx::query!("UPDATE admins SET failed_login_count = 4 WHERE id = $1", id)
        .execute(&pool)
        .await
        .unwrap();
    store
        .save_code(PHONE, Purpose::AdminLogin, CODE, ttl())
        .await
        .unwrap();

    let (status, _, _) = admin_login(&state, login_body(PHONE, PASSWORD, WRONG_CODE)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "第 5 次失败本身仍 401");
    assert!(
        locked_until(&pool, PHONE)
            .await
            .is_some_and(|t| t > chrono::Utc::now()),
        "第 5 次失败应触发锁定（locked_until 进未来）"
    );

    // 锁定生效：随后三因子全对也 423
    let (status, _, _) = admin_login(&state, login_body(PHONE, PASSWORD, CODE)).await;
    assert_eq!(status, StatusCode::LOCKED, "触锁后正确凭证也应 423");
}

// ============================== 归一化 & 基础设施 ==============================

#[sqlx::test]
async fn phone_is_normalized_before_code_verification(pool: PgPool) {
    // 回归锁：发码按归一化手机号存 key；登录请求带首尾空格。
    // 密码路径 normalize 了而 verify 用原始串的话，key 对不上 → 永远 401 还累计——本用例立红。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    create_admin(&pool, PHONE).await;
    store
        .save_code(PHONE, Purpose::AdminLogin, CODE, ttl())
        .await
        .unwrap();

    let padded = format!("  {PHONE}  ");
    let (status, _, body) = admin_login(&state, login_body(&padded, PASSWORD, CODE)).await;
    assert_eq!(status, StatusCode::OK, "带空格手机号应归一化后通过：{body}");
}

#[sqlx::test]
async fn otp_backend_down_is_503_not_401(pool: PgPool) {
    // Redis 挂 = 验不了码 ⇒ fail-close 503。绝不能是 401（骗用户「你输错了」，
    // 且供应商抖动期间管理员会被累计误锁）、也不能是 500（这不是 bug 是依赖故障）。
    let (mut state, _store) = AppState::for_test_with_otp_store(pool.clone());
    create_admin(&pool, PHONE).await;
    // 换上指向死地址的 otp_service（端口 1 无服务，惰性建连、用时才失败）
    let dead = deadpool_redis::Config::from_url("redis://127.0.0.1:1/")
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .expect("惰性 pool 应能创建");
    state.otp_service = Arc::new(OtpService::new(
        OtpStore::new(dead),
        OtpSender::Mock,
        Duration::from_secs(60),
        10,
        ttl(),
        MAX_ATTEMPTS,
    ));

    let (status, _, _) = admin_login(&state, login_body(PHONE, PASSWORD, CODE)).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "OTP 后端故障应 503 fail-close"
    );
    assert_eq!(
        failed_count(&pool, PHONE).await,
        0,
        "基础设施故障不是用户的错，不得累计失败"
    );
}
