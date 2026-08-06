//! `POST /otp/send` handler 集成测试（真 PG + 真 Redis + `OtpSender::Mock`）。
//!
//! 只验 **handler 层独有的翻译**：入参校验、`OtpServiceError`→HTTP 映射、限流状态码、不泄露码。
//! 编排/限流本身的正确性在 tests/otp_service.rs。
//!
//! 状态码契约（本文件钉死）：
//! - 合法请求 → **202**；成功体**不含**验证码
//! - phone/email 都不给 / 都给 / 给空串 → **400**（handler 手写校验）
//! - `purpose` 缺失或非法枚举值 → **422**（axum `Json` 反序列化失败，非 handler 校验）
//! - 冷却窗内二次 → **429**；Redis 不可达 → **503**（fail-close）
//! - Send 失败→503 **未覆盖**（`OtpSender::Mock` 永不失败，需 test-only 失败 sender，暂缺）
//!
//! ⚠️ 这些测试**硬依赖本地活 Redis**（默认 127.0.0.1:6379）：OTP 路径不碰 PG，`#[sqlx::test]`
//! 只隔离 PG；跨用例隔离靠 `for_test` 的**每次调用唯一 key 前缀**。Redis 挂则所有 202 变 503。

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

use tsz_rust::otp::model::Purpose;
use tsz_rust::state::AppState;

/// POST /otp/send，返回 (状态码, 响应体文本)。每次用 `state.clone()` 新建 router（oneshot 消费它）；
/// `otp_service` 是 Arc，克隆共享同一实例 → 同一 state 上多次请求**共享限流状态**。
async fn post_send(state: &AppState, body: Value) -> (StatusCode, String) {
    let resp = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/otp/send")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn status_of(state: &AppState, body: Value) -> StatusCode {
    post_send(state, body).await.0
}

/// 文本里是否有连续 6 位数字（用来抓「码泄进响应体」的回归）。
fn has_six_digit_run(s: &str) -> bool {
    s.as_bytes()
        .windows(6)
        .any(|w| w.iter().all(u8::is_ascii_digit))
}

// ============================ 成功路径 ============================

#[sqlx::test]
async fn valid_request_is_accepted_and_leaks_no_code(pool: PgPool) {
    let state = AppState::for_test(pool);
    let (status, body) =
        post_send(&state, json!({"phone": "13800000000", "purpose": "login"})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "合法请求应 202 Accepted");
    // 码是本域机密（Mock 只把它打进日志）——响应体绝不能带 6 位码。
    assert!(
        !has_six_digit_run(&body),
        "成功响应体不应含验证码，实得 {body:?}"
    );
}

#[sqlx::test]
async fn email_target_is_accepted(pool: PgPool) {
    let state = AppState::for_test(pool);
    let status = status_of(
        &state,
        json!({"email": "user@example.com", "purpose": "login"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "email target 也应 202");
}

// ============================ 限流 ============================

#[sqlx::test]
async fn second_request_in_cooldown_is_429(pool: PgPool) {
    let state = AppState::for_test(pool);
    let body = json!({"phone": "13800000000", "purpose": "login"});
    assert_eq!(
        status_of(&state, body.clone()).await,
        StatusCode::ACCEPTED,
        "首次应 202"
    );
    assert_eq!(
        status_of(&state, body).await,
        StatusCode::TOO_MANY_REQUESTS,
        "冷却窗内二次请求应 429"
    );
}

#[sqlx::test]
async fn cooldown_is_scoped_to_purpose(pool: PgPool) {
    // 自守卫：先证明 login 冷却确实生效（二次 429），再证明它不外溢到 password_reset。
    // 单看「两个都 202」会在限流被整体关掉时也通过——加一条 429 断言堵住这个假绿。
    let state = AppState::for_test(pool);
    assert_eq!(
        status_of(&state, json!({"phone": "13800000000", "purpose": "login"})).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        status_of(&state, json!({"phone": "13800000000", "purpose": "login"})).await,
        StatusCode::TOO_MANY_REQUESTS,
        "login 冷却应确实生效"
    );
    assert_eq!(
        status_of(
            &state,
            json!({"phone": "13800000000", "purpose": "password_reset"})
        )
        .await,
        StatusCode::ACCEPTED,
        "password_reset 不应被 login 的冷却挡住"
    );
}

// ============================ 入参校验（400） ============================

#[sqlx::test]
async fn missing_target_is_400(pool: PgPool) {
    let state = AppState::for_test(pool);
    let status = status_of(&state, json!({"purpose": "login"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "缺 phone/email 应 400");
}

#[sqlx::test]
async fn empty_target_is_400(pool: PgPool) {
    // {"phone":""} 不能被当作合法 target 下发（会污染 key 空间、发无意义短信）。
    // handler 须把空串归一成「没给」——对齐 register 的 `.filter(|s| !s.is_empty())`。
    let state = AppState::for_test(pool);
    let status = status_of(&state, json!({"phone": "", "purpose": "login"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "空串 target 应 400");
}

#[sqlx::test]
async fn invalid_email_reports_stable_code_and_field(pool: PgPool) {
    let state = AppState::for_test(pool);
    let (status, body) =
        post_send(&state, json!({"email": "not-an-email", "purpose": "login"})).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_str(&body).expect("错误响应应为 JSON");
    assert_eq!(body["code"], "invalid_email");
    assert_eq!(body["field"], "email");
}

#[sqlx::test]
async fn both_phone_and_email_is_400(pool: PgPool) {
    // 一条码只能发一个渠道；两者都给 → 歧义，拒绝。
    let state = AppState::for_test(pool);
    let status = status_of(
        &state,
        json!({"phone": "13800000000", "email": "user@example.com", "purpose": "login"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "phone 与 email 二选一，都给应 400"
    );
}

// ============================ purpose 反序列化（422） ============================

#[sqlx::test]
async fn missing_purpose_is_422(pool: PgPool) {
    // purpose 是必填字段：缺了 → axum Json 反序列化失败 → 422（非 handler 校验的 400）。
    let state = AppState::for_test(pool);
    let status = status_of(&state, json!({"phone": "13800000000"})).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "缺 purpose 应 422"
    );
}

#[sqlx::test]
async fn unknown_purpose_is_422(pool: PgPool) {
    // 非法枚举值 → 反序列化失败 → 422。
    let state = AppState::for_test(pool);
    let status = status_of(&state, json!({"phone": "13800000000", "purpose": "nope"})).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "非法 purpose 应 422"
    );
}

// ================ admin 用途隔离：公开端点铸不出 admin 码（安全回归） ================
//
// 背景（code-review #1）：`/otp/send` 曾接受客户端传入的 `purpose: Purpose`（含 admin_login），
// 而 admin 登录 verify 读的是同一 Redis keyspace（otp:code:<target>:admin_login）。
// 于是任何人 POST {"phone":任意号,"purpose":"admin_login"} 就能在公网铸出一枚 admin 码，
// 并刷爆某管理员的 admin_login 冷却/日限（login-code 把 RateLimited 压成 202，管理员看不到异常）。
// C 方案修复：公开端点改用只含公开用途的 `PublicOtpPurpose`，`Purpose` 摘掉 Deserialize——
// admin_login 在类型上就无法从 body 反序列化出来。这两条测试钉死该不变量。

#[sqlx::test]
async fn public_send_cannot_request_admin_login_purpose(pool: PgPool) {
    // admin_login 对公开端点是非法枚举值 → 反序列化失败 → 422（与 unknown_purpose_is_422 同语义）。
    let (state, store) = AppState::for_test_with_otp_store(pool);
    let (status, _) = post_send(
        &state,
        json!({"phone": "13800138000", "purpose": "admin_login"}),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "公开端点不得接受 admin_login 用途（应视为非法枚举值 422）"
    );
    // 载荷性断言：不只看状态码，要证明 Redis 里**确实没落 admin 码**——
    // 挡住「返 422 但 save 已先发生」这类假修复。
    assert!(
        !store
            .code_exists("13800138000", Purpose::AdminLogin)
            .await
            .unwrap(),
        "公开端点被拒后不得在 admin_login keyspace 留下任何码"
    );
}

#[sqlx::test]
async fn public_send_still_mints_public_purpose_code(pool: PgPool) {
    // 正向对照：同一个 store、同一套断言机制下，公开用途 login 必须真落码。
    // 否则上一条的 code_exists==false 可能只是「这机制恒为假」的假绿。
    let (state, store) = AppState::for_test_with_otp_store(pool);
    let (status, _) = post_send(&state, json!({"phone": "13800138000", "purpose": "login"})).await;

    assert_eq!(status, StatusCode::ACCEPTED, "公开用途 login 应 202");
    assert!(
        store
            .code_exists("13800138000", Purpose::Login)
            .await
            .unwrap(),
        "login 应真正落码（对照 code_exists 机制本身工作正常）"
    );
}

// ============================ fail-close（503） ============================

#[sqlx::test]
async fn redis_down_is_503(pool: PgPool) {
    // 注入一个指向死地址的 redis pool → request 的第一步 check_cooldown 即拿不到连接 →
    // OtpServiceError::Store → handler 映射 503。这是唯一守住 fail-close（Redis 抖动不放行）的用例。
    let dead = tsz_rust::platform::connect_redis("redis://127.0.0.1:1")
        .await
        .expect("惰性建池即使地址不可达也应 Ok");
    let state = AppState::for_test_with_redis(pool, dead);
    let status = status_of(&state, json!({"phone": "13800000000", "purpose": "login"})).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "Redis 不可达时应 fail-close 503"
    );
}
