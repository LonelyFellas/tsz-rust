//! `POST /api/v1/admin/auth/refresh`（admin 会话轮换）端到端测试。
//!
//! 本端点无 Redis 依赖（不碰验证码），故直接用 `AppState::for_test` + 由
//! `AdminSessionService::issue` 预铸一枚 refresh 明文塞进 Cookie 头驱动 handler，
//! 不走 login 全链。只测对外行为，不锁内部分层。
//!
//! 编排契约（本文件钉死，与 handler 的编号步骤对应）：
//!
//! ```text
//! ① 无 cookie / 垃圾串 / 查无此枚 ⇒ 401，逐字节同一文案（不泄露「这枚曾有效」）
//! ② disabled ⇒ 403；且 rotate 是压轴步——旧枚绝不因这次 403 被消费
//! ③ locked   ⇒ 423；同样不消费旧枚（临时锁定不该把人踢下线）
//! ④ 成功 ⇒ 200 + 新 access token + 经 Set-Cookie 下发的新 refresh；
//!    Q8 绝对死线：新枚 expires_at **继承**旧枚、不重算（轮换不续命）；
//!    旧枚原子作废——同一枚再刷即 401（不可重放）
//! ```

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Duration;
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::admin::{
    ADMIN_REFRESH_TOKEN_COOKIE, AdminRefreshTokenRepository, AdminRepository, AdminRole,
    AdminSessionService, IssuedAdminRefresh, NewAdmin,
};
use tsz_rust::state::AppState;

/// FK 依赖：造一个 active 管理员（status 走 DB 默认 'active'），返回 id。
async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.to_string(),
            display_name: "测试管理员".to_owned(),
            password_hash: "hashed-pw".to_owned(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin 应成功");
    id
}

/// 为 admin 预铸一枚 refresh（落库 + 返回明文/死线），供后续塞 Cookie。
async fn issue_refresh(pool: &PgPool, ttl: Duration, admin_id: Uuid) -> IssuedAdminRefresh {
    AdminSessionService::new(AdminRefreshTokenRepository::new(pool.clone()), ttl)
        .issue(&admin_id)
        .await
        .expect("issue refresh 应成功")
}

/// POST /api/v1/admin/auth/refresh；`cookie` 为 None 时不带 Cookie 头。
/// 返回 (状态码, Set-Cookie, body 文本)。
async fn refresh(state: &AppState, cookie: Option<&str>) -> (StatusCode, Option<String>, String) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/auth/refresh");
    if let Some(token) = cookie {
        builder = builder.header(
            header::COOKIE,
            format!("{ADMIN_REFRESH_TOKEN_COOKIE}={token}"),
        );
    }
    let resp = tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
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

// ============================== ④ 成功路径 ==============================

#[sqlx::test]
async fn valid_refresh_rotates_inherits_deadline_and_sets_new_cookie(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool).await;
    let issued = issue_refresh(&pool, state.admin_refresh_ttl, id).await;

    let (status, set_cookie, body) = refresh(&state, Some(&issued.plaintext)).await;

    assert_eq!(status, StatusCode::OK, "有效 refresh 应 200：{body}");
    let json: Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["access_token"].as_str().is_some_and(|s| !s.is_empty()),
        "顶层应有非空 access_token"
    );
    assert!(
        json.get("refresh_token").is_none(),
        "新 refresh 只走 cookie，不得进 body"
    );

    // Q8：新枚 expires_at 必须**继承**旧枚绝对死线，轮换不续命。
    assert_eq!(
        json["refresh_token_expires_at"].as_i64(),
        Some(issued.expires_at.timestamp()),
        "轮换出的死线应继承旧枚（不是 now+ttl 重算）"
    );

    // 新 cookie 的安全属性与隔离 path。
    let cookie = set_cookie
        .as_deref()
        .expect("应下发新 admin refresh cookie");
    assert!(
        cookie.starts_with("admin_refresh_token=") && cookie.contains("HttpOnly"),
        "cookie 应为 admin_refresh_token 且 HttpOnly：{cookie}"
    );
    assert!(
        cookie.contains("Path=/api/v1/admin/auth"),
        "cookie Path 应钉在 admin auth 挂载点：{cookie}"
    );

    // 旧枚已被原子消费：同一枚再刷即 401（窗口内按丢包宽待，仍是 401，不可重放）。
    let (status_replay, _, _) = refresh(&state, Some(&issued.plaintext)).await;
    assert_eq!(
        status_replay,
        StatusCode::UNAUTHORIZED,
        "旧 refresh 轮换后应作废，重放应 401"
    );
}

// ============================== ① 无效输入 ==============================

#[sqlx::test]
async fn missing_cookie_is_401(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let (status, set_cookie, _) = refresh(&state, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "缺 cookie 应 401");
    assert!(set_cookie.is_none(), "失败不应下发任何 refresh cookie");
}

#[sqlx::test]
async fn unknown_token_is_401(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    // 查无此枚（垃圾串，从未落库）——与「无 cookie」同样 401，不区分。
    let (status, set_cookie, _) = refresh(&state, Some("this-token-never-existed")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "未知 token 应 401");
    assert!(set_cookie.is_none(), "失败不应下发任何 refresh cookie");
}

// ============================== ② disabled ==============================

#[sqlx::test]
async fn disabled_admin_is_403_and_token_not_consumed(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool).await;
    let issued = issue_refresh(&pool, state.admin_refresh_ttl, id).await;
    sqlx::query!("UPDATE admins SET status = 'disabled' WHERE id = $1", id)
        .execute(&pool)
        .await
        .unwrap();

    let (status, set_cookie, _) = refresh(&state, Some(&issued.plaintext)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "禁用账号刷新应 403");
    assert!(set_cookie.is_none(), "403 不应下发新 refresh cookie");

    // rotate 是压轴步：403 挡在其之前，旧枚绝不被消费——恢复 active 后原枚仍可刷。
    sqlx::query!("UPDATE admins SET status = 'active' WHERE id = $1", id)
        .execute(&pool)
        .await
        .unwrap();
    let (status_after, _, _) = refresh(&state, Some(&issued.plaintext)).await;
    assert_eq!(
        status_after,
        StatusCode::OK,
        "禁用期间不该消费旧枚：恢复后同一枚应仍能轮换"
    );
}

// ============================== ③ 锁定 ==============================

#[sqlx::test]
async fn locked_admin_is_423_and_token_not_consumed(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool).await;
    let issued = issue_refresh(&pool, state.admin_refresh_ttl, id).await;
    sqlx::query!(
        "UPDATE admins SET locked_until = NOW() + interval '10 minutes' WHERE id = $1",
        id
    )
    .execute(&pool)
    .await
    .unwrap();

    let (status, set_cookie, _) = refresh(&state, Some(&issued.plaintext)).await;
    assert_eq!(status, StatusCode::LOCKED, "锁定账号刷新应 423");
    assert!(set_cookie.is_none(), "423 不应下发新 refresh cookie");

    // 临时锁定不该把人踢下线：解锁后原枚仍可刷（rotate 压轴，未被消费）。
    sqlx::query!("UPDATE admins SET locked_until = NULL WHERE id = $1", id)
        .execute(&pool)
        .await
        .unwrap();
    let (status_after, _, _) = refresh(&state, Some(&issued.plaintext)).await;
    assert_eq!(
        status_after,
        StatusCode::OK,
        "锁定期间不该消费旧枚：解锁后同一枚应仍能轮换"
    );
}
