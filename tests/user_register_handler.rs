//! `POST /user/register` handler 的端到端行为测试（真库 + `oneshot`）。
//!
//! 这层只测 **handler 自己负责的翻译**，不重测 service 的业务规则：
//!   1. 领域结果/错误 → HTTP 状态码（201 / 400 / 409）的映射；
//!   2. 响应体只暴露**安全字段**、绝不泄露 `password_hash`；
//!   3. 对外错误文案是 handler 里写死的那几句（契约的一部分）。
//!
//! service 的语义细节（昵称随机生成、密码 bcrypt、归一化、判重靠 DB 唯一约束）
//! 已在 `tests/user_service.rs` 全绿覆盖，这里不再重复——只确认它们经由 HTTP
//! 暴露出来的**观感**没错。
//!
//! ⚠️ 对齐的 handler 契约（改了 handler 记得同步这里）：
//!   - 路由 `POST /user/register`，请求体 JSON `{phone?, email?, password}`；
//!   - 成功 `201` + `{user_id, display_name, role}`；
//!   - 缺 phone&email → `400 {"error":"phone or email is missing"}`；
//!   - 空密码 → `400 {"error":"password is missing"}`；
//!   - 其余密码不合规 → `400 {"error":"invalid password"}`；
//!   - 手机/邮箱已占 → `409 {"error":"user already exists"}`。

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

/// 把 JSON 值打成 `POST /user/register` 请求，过 `router().oneshot()`，
/// 返回 `(状态码, 响应体 JSON)`。成功体和错误体都是 JSON，统一解析。
async fn register(pool: PgPool, body: Value) -> (StatusCode, Value) {
    let resp = tsz_rust::router(tsz_rust::state::AppState::for_test(pool))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/user/register")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    // 空体（理论上不会有）兜底成 Null，避免 unwrap 崩测试。
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

// ————————————————————— 成功主线 —————————————————————

/// 手机 + 邮箱齐全 → 201，响应只含安全字段，role 恒为 student，且**绝不含哈希**。
#[sqlx::test]
async fn register_returns_201_with_safe_body(pool: PgPool) {
    let (status, body) = register(
        pool,
        json!({
            "phone": "13800138000",
            "email": "alice@example.com",
            "password": "password123",
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "注册成功应返回 201");

    // user_id 应是合法 UUID 字符串（DB 用 UUIDv7 主键）。
    let user_id = body["user_id"].as_str().expect("响应应含 user_id 字符串");
    assert!(
        uuid::Uuid::parse_str(user_id).is_ok(),
        "user_id 应是合法 UUID：{user_id}"
    );

    // 默认昵称由后端随机生成，非空即可（内容随机，不断言具体值）。
    assert!(
        !body["display_name"].as_str().unwrap_or("").is_empty(),
        "display_name 应非空"
    );

    // 注册永远是 student（老师须系统内申请，请求里无 role 字段可选）。
    assert_eq!(body["role"], "student", "注册用户角色恒为 student");

    // —— 核心安全断言：响应绝不能泄露口令相关字段 ——
    assert!(body.get("password").is_none(), "响应不得含 password 字段");
    assert!(
        body.get("password_hash").is_none(),
        "响应不得含 password_hash 字段"
    );
    // 更狠一层：整个响应体里不得出现 bcrypt 哈希前缀（防将来加字段误带出去）。
    let raw = body.to_string();
    assert!(
        !raw.contains("$2b$") && !raw.contains("$2a$") && !raw.contains("$2y$"),
        "响应体不得出现 bcrypt 哈希片段：{raw}"
    );
}

/// 只给手机号也能注册（邮箱可选）。
#[sqlx::test]
async fn register_succeeds_with_phone_only(pool: PgPool) {
    let (status, body) =
        register(pool, json!({ "phone": "13800138000", "password": "password123" })).await;

    assert_eq!(status, StatusCode::CREATED, "仅手机号应能注册成功");
    assert_eq!(body["role"], "student");
}

/// 只给邮箱也能注册（手机号可选）。
#[sqlx::test]
async fn register_succeeds_with_email_only(pool: PgPool) {
    let (status, body) = register(
        pool,
        json!({ "email": "alice@example.com", "password": "password123" }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "仅邮箱应能注册成功");
    assert_eq!(body["role"], "student");
}

// ————————————————————— 400：主体缺失 —————————————————————

/// phone 和 email **都不给** → 400，文案固定。
#[sqlx::test]
async fn missing_phone_and_email_returns_400(pool: PgPool) {
    let (status, body) = register(pool, json!({ "password": "password123" })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "缺主体标识应 400");
    assert_eq!(
        body["error"].as_str(),
        Some("phone or email is missing"),
        "错误文案应对齐 handler 契约"
    );
}

/// phone/email **给了空串** → service 归一化成 None → 与「都不给」等价 → 400。
/// 这条专门网住「传了 `\"\"` 但没真值」这种前端常见畸形输入。
#[sqlx::test]
async fn empty_phone_and_email_returns_400(pool: PgPool) {
    let (status, body) =
        register(pool, json!({ "phone": "", "email": "", "password": "password123" })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "空串标识应等价于缺失 → 400");
    assert_eq!(body["error"].as_str(), Some("phone or email is missing"));
}

// ————————————————————— 400：密码不合规 —————————————————————

/// 空密码 → 走 `PasswordError::Empty` 分支，文案与「太短/太长」不同。
#[sqlx::test]
async fn empty_password_returns_400(pool: PgPool) {
    let (status, body) =
        register(pool, json!({ "phone": "13800138000", "password": "" })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"].as_str(),
        Some("password is missing"),
        "空密码应报 password is missing"
    );
}

/// 太短（<8）→ 400，走通用 `invalid password` 文案（不区分具体原因，防探测）。
#[sqlx::test]
async fn short_password_returns_400(pool: PgPool) {
    let (status, body) =
        register(pool, json!({ "phone": "13800138000", "password": "short" })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"].as_str(), Some("invalid password"));
}

/// 太长（>72 字节，bcrypt 上限）→ 400，同样是 `invalid password`。
#[sqlx::test]
async fn too_long_password_returns_400(pool: PgPool) {
    let long = "a".repeat(73);
    let (status, body) =
        register(pool, json!({ "phone": "13800138000", "password": long })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"].as_str(), Some("invalid password"));
}

// ————————————————————— 409：唯一冲突 —————————————————————

/// 同一手机号注册两次 → 第二次 409（DB 唯一约束 → UserAlreadyExists → Conflict）。
/// 第二次故意换邮箱，确保冲突确实由 phone 触发，不是邮箱撞了。
#[sqlx::test]
async fn duplicate_phone_returns_409(pool: PgPool) {
    let (first, _) = register(
        pool.clone(),
        json!({ "phone": "13800138000", "email": "a@example.com", "password": "password123" }),
    )
    .await;
    assert_eq!(first, StatusCode::CREATED, "首次注册应成功");

    let (second, body) = register(
        pool,
        json!({ "phone": "13800138000", "email": "b@example.com", "password": "password123" }),
    )
    .await;

    assert_eq!(second, StatusCode::CONFLICT, "手机号已占用应 409");
    assert_eq!(body["error"].as_str(), Some("user already exists"));
}

/// 同一邮箱注册两次 → 第二次 409。第二次换手机号，确保冲突由 email 触发。
#[sqlx::test]
async fn duplicate_email_returns_409(pool: PgPool) {
    let (first, _) = register(
        pool.clone(),
        json!({ "phone": "13800138000", "email": "alice@example.com", "password": "password123" }),
    )
    .await;
    assert_eq!(first, StatusCode::CREATED, "首次注册应成功");

    let (second, body) = register(
        pool,
        json!({ "phone": "13900139000", "email": "alice@example.com", "password": "password123" }),
    )
    .await;

    assert_eq!(second, StatusCode::CONFLICT, "邮箱已占用应 409");
    assert_eq!(body["error"].as_str(), Some("user already exists"));
}
