//! `POST /api/v1/admin/auth/login-code`（admin 发码端点）反枚举测试。
//!
//! 契约（admin-design.md §6 发码端点，本文件钉死）：
//!
//! - **恒 202 空 body 压平一切非基础设施结果**：查无此号 / 已禁用 / 冷却中
//!   四态响应逐字节一致——admin 端点公网可达，任何响应差异（哪怕 429 与 202 之差）
//!   都是「这个手机号是不是管理员」的名录枚举面。前端本地 60s 倒计时，不依赖 429。
//!   （**与 web `/otp/send` 的 429 语义刻意不同**，§15 偏离 9。）
//! - **只有 active admin 才真正落码**：其余分支静默返回、绝不触发发送——
//!   否则接上真短信后，这里就是「给任意手机号发短信」的成本攻击面。
//! - 仅基础设施（Redis/sender）故障返 **503**（fail-close，无需也不应隐藏）。
//!
//! 落码断言走 `OtpStore::code_exists`（只读 test seam）：Mock sender 不回传码，
//! 只能验键存在性。⚠️ 硬依赖本地活 Redis。

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::json;
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

/// 建一个可登录的 active admin（与 admin_login_handler 同款 helper）。
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

/// POST /api/v1/admin/auth/login-code，返回 (状态码, 原始 body 文本)。
async fn login_code(state: &AppState, phone: &str) -> (StatusCode, String) {
    let resp = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/auth/login-code")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "phone": phone }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

// ============================== 真发分支 ==============================

#[sqlx::test]
async fn active_admin_gets_202_and_code_lands(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    create_admin(&pool, PHONE).await;

    let (status, body) = login_code(&state, PHONE).await;

    assert_eq!(status, StatusCode::ACCEPTED, "契约 202，不是 200");
    assert!(body.is_empty(), "body 应为空：{body}");
    assert!(
        store.code_exists(PHONE, Purpose::AdminLogin).await.unwrap(),
        "active admin 应真正落码（purpose=admin_login）"
    );
}

// ============================== 静默压平分支 ==============================

#[sqlx::test]
async fn unknown_phone_is_silent_202_and_no_code(pool: PgPool) {
    // 一个 admin 都不建：查无此号必须静默 202 且**绝不落码/发短信**。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());

    let (status, body) = login_code(&state, "13999990000").await;

    assert_eq!(status, StatusCode::ACCEPTED, "查无此号也应 202（反枚举）");
    assert!(body.is_empty());
    assert!(
        !store
            .code_exists("13999990000", Purpose::AdminLogin)
            .await
            .unwrap(),
        "查无此号不得落码——否则这是给任意号码发短信的成本攻击面"
    );
}

#[sqlx::test]
async fn disabled_admin_is_silent_202_and_no_code(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let id = create_admin(&pool, PHONE).await;
    sqlx::query!("UPDATE admins SET status = 'disabled' WHERE id = $1", id)
        .execute(&pool)
        .await
        .unwrap();

    let (status, body) = login_code(&state, PHONE).await;

    assert_eq!(status, StatusCode::ACCEPTED, "禁用账号也应 202（不暴露禁用态）");
    assert!(body.is_empty());
    assert!(
        !store.code_exists(PHONE, Purpose::AdminLogin).await.unwrap(),
        "禁用账号不得落码"
    );
}

#[sqlx::test]
async fn locked_admin_is_silent_202_and_no_code(pool: PgPool) {
    // #4：locked_until 在未来的 admin（status 仍 Active）打 login-code——必须静默不发码。
    // 理由：该 admin 锁定期内 login 一律 423（码全对也拒），发码是纯浪费 + 真短信成本 + 骚扰面。
    // 归入与 disabled/查无此号**同一个沉默 202 桶**（反枚举字节一致），且不落码。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let id = create_admin(&pool, PHONE).await;
    sqlx::query!(
        "UPDATE admins SET locked_until = NOW() + interval '10 minutes' WHERE id = $1",
        id
    )
    .execute(&pool)
    .await
    .unwrap();

    let (status, body) = login_code(&state, PHONE).await;

    assert_eq!(status, StatusCode::ACCEPTED, "锁定 admin 也应静默 202（不暴露锁定态）");
    assert!(body.is_empty());
    assert!(
        !store.code_exists(PHONE, Purpose::AdminLogin).await.unwrap(),
        "锁定中的 admin 不得落码——发码门禁须与 login 一致地排除锁定账号（#4）"
    );
}

#[sqlx::test]
async fn expired_lock_admin_gets_code(pool: PgPool) {
    // 回归对照：locked_until 已过期（自动解锁）的 active admin 必须能正常收码——
    // 防止 #4 的修复过度拦成「凡有 locked_until 就不发」而误伤已解锁账号。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let id = create_admin(&pool, PHONE).await;
    sqlx::query!(
        "UPDATE admins SET locked_until = NOW() - interval '1 minute' WHERE id = $1",
        id
    )
    .execute(&pool)
    .await
    .unwrap();

    let (status, _) = login_code(&state, PHONE).await;

    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(
        store.code_exists(PHONE, Purpose::AdminLogin).await.unwrap(),
        "锁已过期的 active admin 应正常落码"
    );
}

#[sqlx::test]
async fn cooldown_is_flattened_to_202(pool: PgPool) {
    // 60s 冷却内连发两次：web /otp/send 会 429，admin 这里必须压平成 202。
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    create_admin(&pool, PHONE).await;

    let (first, _) = login_code(&state, PHONE).await;
    assert_eq!(first, StatusCode::ACCEPTED, "第一次应 202 并落码");

    let (second, body) = login_code(&state, PHONE).await;
    assert_eq!(
        second,
        StatusCode::ACCEPTED,
        "冷却中也应 202（不是 429——前端本地倒计时，不依赖服务端反馈）"
    );
    assert!(body.is_empty());
    // 第一枚码不受第二次请求影响，仍然在
    assert!(
        store.code_exists(PHONE, Purpose::AdminLogin).await.unwrap(),
        "冷却中原码应仍在"
    );
}

#[sqlx::test]
async fn all_silent_branches_are_byte_identical(pool: PgPool) {
    // 四态（真发/查无此号/禁用/冷却）响应逐字节一致：202 + 空 body。
    // 只要有一态长得不一样，就是可探测的 oracle。
    let (state, _store) = AppState::for_test_with_otp_store(pool.clone());
    let id = create_admin(&pool, PHONE).await;
    let disabled_phone = "13800138001";
    let id2 = create_admin(&pool, disabled_phone).await;
    assert_ne!(id, id2);
    sqlx::query!("UPDATE admins SET status = 'disabled' WHERE id = $1", id2)
        .execute(&pool)
        .await
        .unwrap();

    let real = login_code(&state, PHONE).await; // 真发
    let cooldown = login_code(&state, PHONE).await; // 冷却中
    let unknown = login_code(&state, "13999990000").await; // 查无此号
    let disabled = login_code(&state, disabled_phone).await; // 禁用

    assert_eq!(real, cooldown, "真发与冷却应逐字节一致");
    assert_eq!(cooldown, unknown, "冷却与查无此号应逐字节一致");
    assert_eq!(unknown, disabled, "查无此号与禁用应逐字节一致");
    assert_eq!(real.0, StatusCode::ACCEPTED);
    assert!(real.1.is_empty(), "统一空 body");
}

// ============================== 基础设施故障 ==============================

#[sqlx::test]
async fn redis_down_for_active_admin_is_503(pool: PgPool) {
    // active admin 的真发路径碰 Redis，Redis 挂 → 503 fail-close（基础设施故障不隐藏）。
    let (mut state, _store) = AppState::for_test_with_otp_store(pool.clone());
    create_admin(&pool, PHONE).await;
    let dead = deadpool_redis::Config::from_url("redis://127.0.0.1:1/")
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .expect("惰性 pool 应能创建");
    state.otp_service = Arc::new(OtpService::new(
        OtpStore::new(dead),
        OtpSender::Mock,
        Duration::from_secs(60),
        10,
        Duration::from_secs(300),
        5,
    ));

    let (status, body) = login_code(&state, PHONE).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "Redis 挂应 503，不是 500/202：{body}"
    );
    // 503 的 body 不泄内部原因（error.rs 惯例），这里只钉不含连接串
    assert!(
        !body.contains("127.0.0.1"),
        "故障响应不得泄露基础设施地址：{body}"
    );
}
