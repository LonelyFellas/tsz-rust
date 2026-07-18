//! `POST /auth/refresh` 与 `POST /auth/logout` handler 的端到端测试（真库 + `oneshot`）。
//!
//! 契约 0.2：refresh token 走 httpOnly Cookie——入参从 `Cookie` 头读、新枚经 `Set-Cookie` 下发、
//! **body 里不得出现 refresh token 明文**。
//! 验 handler 层的编排与翻译：轮换出新 cookie、旧 token 单次使用即失效、失效态不可区分、
//! 禁用账号发不出 token、登出清 cookie 且吊销、缺 cookie 的行为。
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

/// 通用 POST：`refresh` 为 Some 时模拟浏览器带上 refresh cookie；`body` 为 None 时不带请求体
/// （refresh/logout 的新契约就是无 body）。返回 (状态码, Set-Cookie 头, 响应体 JSON)。
async fn post(
    pool: PgPool,
    uri: &str,
    refresh: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Option<String>, Value) {
    let mut builder = Request::builder().method("POST").uri(uri);
    if let Some(token) = refresh {
        builder = builder.header(header::COOKIE, format!("refresh_token={token}"));
    }
    let request = match body {
        Some(json) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json.to_string())),
        None => builder.body(Body::empty()),
    }
    .unwrap();

    let resp = tsz_rust::router(AppState::for_test(pool))
        .oneshot(request)
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

/// 从 `Set-Cookie` 头里取出 refresh token 明文（`refresh_token=<值>; ...`）。
fn cookie_value(set_cookie: &str) -> &str {
    set_cookie
        .strip_prefix("refresh_token=")
        .and_then(|rest| rest.split(';').next())
        .expect("Set-Cookie 应以 refresh_token= 开头")
}

/// 登录拿一枚可用的 refresh token（从 Set-Cookie 里取，用户须已注册）。
async fn login_for_refresh(pool: &PgPool, phone: &str) -> String {
    let (status, set_cookie, _) = post(
        pool.clone(),
        "/api/v1/auth/login",
        None,
        Some(json!({ "identifier": phone, "password": "password123" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "预置登录应成功");
    cookie_value(set_cookie.as_deref().expect("登录应下发 Set-Cookie")).to_owned()
}

// ————————————————————— 成功 + 轮换 —————————————————————

/// 有效 refresh cookie → 200 + access 字段齐全 + `Set-Cookie` 轮换出一枚**新** refresh，
/// 且 body 里不得出现 refresh token 明文。
#[sqlx::test]
async fn refresh_returns_200_with_rotated_cookie(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await;

    let (status, set_cookie, body) = post(pool, "/api/v1/auth/refresh", Some(&r0), None).await;

    assert_eq!(status, StatusCode::OK, "有效 refresh 应 200");
    assert!(
        body["access_token"].as_str().is_some_and(|s| !s.is_empty()),
        "应有非空 access_token"
    );
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["expires_in"], 900, "for_test 的 access TTL=15min=900s");
    assert_eq!(
        body["access_token"].as_str().unwrap().split('.').count(),
        3,
        "access_token 应是三段式 JWT"
    );
    assert!(
        !body.to_string().contains("refresh_token"),
        "refresh token 明文不得出现在响应 body"
    );

    let cookie = set_cookie.as_deref().expect("轮换后应重新下发 Set-Cookie");
    assert_ne!(
        cookie_value(cookie),
        r0,
        "轮换后应发一枚不同于旧的 refresh token"
    );
    assert!(
        cookie.contains("HttpOnly"),
        "轮换下发的 cookie 也必须 HttpOnly：{cookie}"
    );
    assert!(
        cookie.contains(&format!("Path={}", tsz_rust::auth::AUTH_MOUNT)),
        "轮换下发的 cookie Path 必须与登录一致（AUTH_MOUNT）：{cookie}"
    );
}

/// 单次使用：旧 refresh 用一次成功轮换后，再用即 401（旧的一经轮换即作废）。
#[sqlx::test]
async fn old_refresh_is_rejected_after_rotation(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await;

    let (s1, _, _) = post(pool.clone(), "/api/v1/auth/refresh", Some(&r0), None).await;
    assert_eq!(s1, StatusCode::OK, "首次刷新应成功");

    let (s2, set_cookie, body) = post(pool, "/api/v1/auth/refresh", Some(&r0), None).await;
    assert_eq!(s2, StatusCode::UNAUTHORIZED, "已轮换的旧 token 再用应 401");
    assert_eq!(body["error"].as_str(), Some("invalid refresh token"));
    assert!(set_cookie.is_none(), "刷新失败不得下发新 cookie");
}

/// 轮换链能续：用轮换出的**新** refresh cookie 再刷，仍成功。
#[sqlx::test]
async fn rotated_new_refresh_is_usable(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await;

    let (s1, set_cookie, _) = post(pool.clone(), "/api/v1/auth/refresh", Some(&r0), None).await;
    assert_eq!(s1, StatusCode::OK);
    let r1 = cookie_value(set_cookie.as_deref().unwrap()).to_owned();

    let (s2, _, _) = post(pool, "/api/v1/auth/refresh", Some(&r1), None).await;
    assert_eq!(s2, StatusCode::OK, "轮换出的新 refresh 应可继续使用");
}

// ————————————————————— 失败：失效态不可区分 —————————————————————

/// 不带 cookie → 401（新契约下没有 body 兜底，cookie 缺失即未认证）。
#[sqlx::test]
async fn missing_cookie_is_401(pool: PgPool) {
    let (status, _, body) = post(pool, "/api/v1/auth/refresh", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"].as_str(), Some("invalid refresh token"));
}

/// 纯垃圾串 → 401 invalid refresh token。
#[sqlx::test]
async fn garbage_refresh_is_401(pool: PgPool) {
    let (status, _, body) = post(
        pool,
        "/api/v1/auth/refresh",
        Some("definitely-not-a-real-token"),
        None,
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
    let _ = post(pool.clone(), "/api/v1/auth/refresh", Some(&r0), None).await;

    let (s_reused, _, b_reused) = post(pool.clone(), "/api/v1/auth/refresh", Some(&r0), None).await;
    let (s_garbage, _, b_garbage) = post(
        pool,
        "/api/v1/auth/refresh",
        Some("definitely-not-a-real-token"),
        None,
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

/// 重放检测整链（端到端）：已轮换的旧 cookie 再次出现 → 401 且**该用户全部会话连坐吊销**，
/// 另一台设备的合法 cookie 也刷不动 → 全端强制重登（RFC 9700 §4.14.2）。
/// service 层的链吊销细节与宽限窗口规格在 `tests/session_reuse_detection.rs`，这里验 HTTP 全链路接线。
#[sqlx::test]
async fn replayed_cookie_revokes_other_devices_sessions(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let device1 = login_for_refresh(&pool, "13800138000").await;
    let device2 = login_for_refresh(&pool, "13800138000").await; // 第二台设备的独立会话

    // 设备一正常轮换一次，旧枚 device1 进入「已轮换」态
    let (s1, _, _) = post(pool.clone(), "/api/v1/auth/refresh", Some(&device1), None).await;
    assert_eq!(s1, StatusCode::OK, "设备一正常轮换应成功");

    // 把轮换时间回拨出 20 秒宽限窗口——窗口内的重放按丢包重试宽待，不触发连坐。
    // 此时库里唯一 rotated_at 非空的行就是 device1 的旧枚，无需按哈希定位。
    let n = sqlx::query!(
        "UPDATE refresh_tokens SET rotated_at = rotated_at - interval '25 seconds' WHERE rotated_at IS NOT NULL"
    )
    .execute(&pool)
    .await
    .expect("回拨 rotated_at 应成功")
    .rows_affected();
    assert_eq!(n, 1, "应恰好回拨设备一那枚已轮换的旧 cookie");

    // 攻击者重放已轮换的 device1 → 401（对外与普通失效不可区分）
    let (s2, set_cookie, _) =
        post(pool.clone(), "/api/v1/auth/refresh", Some(&device1), None).await;
    assert_eq!(s2, StatusCode::UNAUTHORIZED, "重放应 401");
    assert!(set_cookie.is_none(), "重放不得下发新 cookie");

    // 连坐：设备二的会话也已被吊销，合法 cookie 同样刷不动
    let (s3, _, body) = post(pool, "/api/v1/auth/refresh", Some(&device2), None).await;
    assert_eq!(
        s3,
        StatusCode::UNAUTHORIZED,
        "重放被检测后该用户全部会话应吊销，设备二也必须刷不动"
    );
    assert_eq!(body["error"].as_str(), Some("invalid refresh token"));
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

    let (status, set_cookie, body) = post(pool, "/api/v1/auth/refresh", Some(&r0), None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED, "禁用账号 refresh 应 401");
    assert!(
        body["access_token"].is_null(),
        "禁用账号不应拿到 access_token"
    );
    assert!(set_cookie.is_none(), "禁用账号不应拿到新 refresh cookie");
}

// ————————————————————— 登出 —————————————————————

/// 登出后：204 + `Set-Cookie` 清除 refresh cookie（Max-Age=0），再用该 token 刷新 → 401。
#[sqlx::test]
async fn logout_clears_cookie_and_revokes_token(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await;

    let (s_logout, set_cookie, _) =
        post(pool.clone(), "/api/v1/auth/logout", Some(&r0), None).await;
    assert_eq!(
        s_logout,
        StatusCode::NO_CONTENT,
        "登出应成功（204 No Content）"
    );

    // 清除 cookie：同名同 Path、值为空、Max-Age=0（浏览器收到即删除）
    let cookie = set_cookie.as_deref().expect("登出应下发清除 cookie");
    assert_eq!(cookie_value(cookie), "", "清除 cookie 的值应为空");
    assert!(
        cookie.contains("Max-Age=0"),
        "清除 cookie 应 Max-Age=0：{cookie}"
    );
    assert!(
        cookie.contains(&format!("Path={}", tsz_rust::auth::AUTH_MOUNT)),
        "清除 cookie 的 Path 必须与下发时一致（AUTH_MOUNT），否则浏览器清不掉：{cookie}"
    );

    let (s_refresh, _, body) = post(pool, "/api/v1/auth/refresh", Some(&r0), None).await;
    assert_eq!(s_refresh, StatusCode::UNAUTHORIZED, "登出后再刷应 401");
    assert_eq!(body["error"].as_str(), Some("invalid refresh token"));
}

/// 登出幂等且不泄露：重复登出、以及对从不存在的 token 登出，都应 204 + 清除 cookie。
/// （对齐 RFC 7009 语义：撤销的目标是「确保失效」，对象本就不存在 = 目标已达成。）
#[sqlx::test]
async fn logout_is_idempotent_and_silent(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let r0 = login_for_refresh(&pool, "13800138000").await;

    let (s1, _, _) = post(pool.clone(), "/api/v1/auth/logout", Some(&r0), None).await;
    let (s2, _, _) = post(pool.clone(), "/api/v1/auth/logout", Some(&r0), None).await;
    assert_eq!(s1, StatusCode::NO_CONTENT);
    assert_eq!(s2, StatusCode::NO_CONTENT, "重复登出也应 204（幂等）");

    let (s3, set_cookie, _) = post(pool, "/api/v1/auth/logout", Some("never-existed"), None).await;
    assert_eq!(
        s3,
        StatusCode::NO_CONTENT,
        "登出未知 token 也应 204，不报错"
    );
    assert!(
        set_cookie.is_some_and(|c| c.contains("Max-Age=0")),
        "任何登出路径都应下发清除 cookie"
    );
}

/// 缺 cookie 登出 → 仍是 204（幂等定案，2026-07-18）：
/// 没有 cookie = 已处于登出态 = 目标已达成。典型场景：30 天 Max-Age 到期后
/// 用户点「退出登录」，浏览器不带 cookie——报 401 只会让前端的退出按钮报错。
///
/// 注：此路径**不**要求清除头——`jar.remove` 只对请求里真带来的 cookie 生成
/// 清除 Set-Cookie；浏览器本就没有这枚 cookie，无可清。cookie Path 与 logout
/// 路径同前缀，「有 cookie 却没带上」的状态不存在，清除头的不变量由
/// 上面两条带 cookie 的用例钉住。
#[sqlx::test]
async fn logout_without_cookie_is_204(pool: PgPool) {
    let (status, _, body) = post(pool, "/api/v1/auth/logout", None, None).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "缺 cookie 登出应 204——幂等，不设凭证门槛"
    );
    assert_eq!(body, serde_json::Value::Null, "204 不应有 body");
}

/// logout 只杀当前会话，不迁怒其它设备（logout ≠ 重放连坐的 revoke_all）。
#[sqlx::test]
async fn logout_only_kills_this_session(pool: PgPool) {
    register_user(&pool, "13800138000").await;
    let device1 = login_for_refresh(&pool, "13800138000").await;
    let device2 = login_for_refresh(&pool, "13800138000").await;

    let (s1, _, _) = post(pool.clone(), "/api/v1/auth/logout", Some(&device1), None).await;
    assert_eq!(s1, StatusCode::NO_CONTENT);

    let (s2, _, _) = post(pool, "/api/v1/auth/refresh", Some(&device2), None).await;
    assert_eq!(
        s2,
        StatusCode::OK,
        "设备一登出不该影响设备二的会话——logout 是单会话操作"
    );
}
