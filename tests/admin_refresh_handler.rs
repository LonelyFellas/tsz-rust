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
        .issue(&admin_id, 0)
        .await
        .expect("issue refresh 应成功")
}

/// POST /api/v1/admin/auth/refresh；`cookie` 为 None 时不带 Cookie 头。
/// 返回 (状态码, Set-Cookie, body 文本)。
async fn refresh(state: &AppState, cookie: Option<&str>) -> (StatusCode, Option<String>, String) {
    refresh_with_request(state, cookie, None).await
}

async fn refresh_with_request(
    state: &AppState,
    cookie: Option<&str>,
    request_id: Option<Uuid>,
) -> (StatusCode, Option<String>, String) {
    refresh_with_mode(state, cookie, request_id, false).await
}

async fn refresh_with_mode(
    state: &AppState,
    cookie: Option<&str>,
    request_id: Option<Uuid>,
    current_only: bool,
) -> (StatusCode, Option<String>, String) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/auth/refresh");
    if current_only {
        builder = builder.header("x-refresh-current-only", "v1");
    }
    if let Some(token) = cookie {
        builder = builder.header(
            header::COOKIE,
            format!("{ADMIN_REFRESH_TOKEN_COOKIE}={token}"),
        );
    }
    if let Some(request_id) = request_id {
        builder = builder.header("x-refresh-request-id", request_id.to_string());
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

#[sqlx::test]
async fn lost_refresh_response_can_be_recovered_without_another_rotation(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool).await;
    let issued = issue_refresh(&pool, state.admin_refresh_ttl, id).await;
    let request_id = Uuid::parse_str("283e8512-4e3c-4c59-a02b-2f383a97e9ae").unwrap();
    let first = refresh_with_request(&state, Some(&issued.plaintext), Some(request_id)).await;
    assert_eq!(first.0, StatusCode::OK);
    sqlx::query("UPDATE admin_refresh_tokens SET rotated_at = rotated_at - interval '1 hour' WHERE admin_id = $1 AND rotated_at IS NOT NULL")
        .bind(id).execute(&pool).await.unwrap();
    let recovered = refresh_with_request(&state, Some(&issued.plaintext), Some(request_id)).await;
    assert_eq!(
        recovered.0,
        StatusCode::OK,
        "联网后同一次刷新应恢复：{}",
        recovered.2
    );
    assert_eq!(first.1, recovered.1, "重试应下发同一枚 cookie");
    let new_token = recovered
        .1
        .as_ref()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("admin_refresh_token=")
        .unwrap();
    let body_recovery = refresh_with_request(&state, Some(new_token), Some(request_id)).await;
    assert_eq!(body_recovery.0, StatusCode::OK);
    assert_eq!(
        recovered.1, body_recovery.1,
        "Cookie 已送达而 JSON 丢失时不能再次轮换"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM admin_refresh_tokens WHERE admin_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 2);
}

#[sqlx::test]
async fn recovery_cannot_resurrect_revoked_expired_or_consumed_successors(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let key = Uuid::parse_str("b36adf77-2cb3-4c10-a766-fb2c3a512e19").unwrap();
    for failure in ["revoked", "expired", "consumed"] {
        let id = seed_admin(&pool).await;
        let old = issue_refresh(&pool, state.admin_refresh_ttl, id).await;
        let first = refresh_with_request(&state, Some(&old.plaintext), Some(key)).await;
        assert_eq!(first.0, StatusCode::OK);
        let statement = match failure {
            "revoked" => {
                "UPDATE admin_refresh_tokens SET revoked_at=clock_timestamp() WHERE admin_id=$1 AND rotated_at IS NULL"
            }
            "expired" => {
                "UPDATE admin_refresh_tokens SET expires_at=clock_timestamp()-interval '1 second' WHERE admin_id=$1 AND rotated_at IS NULL"
            }
            _ => {
                "UPDATE admin_refresh_tokens SET rotated_at=clock_timestamp() WHERE admin_id=$1 AND rotated_at IS NULL"
            }
        };
        sqlx::query(statement)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let recovered = refresh_with_request(&state, Some(&old.plaintext), Some(key)).await;
        assert_eq!(
            recovered.0,
            StatusCode::UNAUTHORIZED,
            "{failure} 后继不能复活"
        );
        assert!(recovered.1.is_none());
    }
}

#[sqlx::test]
async fn different_request_key_keeps_replay_detection(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool).await;
    let old = issue_refresh(&pool, state.admin_refresh_ttl, id).await;
    let key = Uuid::parse_str("b36adf77-2cb3-4c10-a766-fb2c3a512e19").unwrap();
    let other = Uuid::parse_str("283e8512-4e3c-4c59-a02b-2f383a97e9ae").unwrap();
    assert_eq!(
        refresh_with_request(&state, Some(&old.plaintext), Some(key))
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE admin_refresh_tokens SET rotated_at=rotated_at-interval '1 hour' WHERE admin_id=$1 AND rotated_at IS NOT NULL")
        .bind(id).execute(&pool).await.unwrap();
    assert_eq!(
        refresh_with_request(&state, Some(&old.plaintext), Some(other))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM admin_refresh_tokens WHERE admin_id=$1 AND revoked_at IS NULL",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live, 0);
}

#[sqlx::test]
async fn recovery_capability_probe_is_non_mutating(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let response = tsz_rust::router(state)
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/api/v1/admin/auth/refresh")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["x-refresh-recovery"], "v1");
    assert_eq!(response.headers()["x-refresh-current-only"], "v1");
    assert!(!response.headers().contains_key(header::SET_COOKIE));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_refresh_tokens")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
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

/// peek 必须保留已轮换但未撤销的凭证，才能执行窗口外重放的全设备吊销。
#[sqlx::test]
async fn rotated_cookie_replay_still_revokes_other_devices(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool).await;
    let old = issue_refresh(&pool, state.admin_refresh_ttl, id).await;
    let svc = AdminSessionService::new(
        AdminRefreshTokenRepository::new(pool.clone()),
        state.admin_refresh_ttl,
    );
    let current = svc.rotate(&old.plaintext).await.unwrap();
    let other = issue_refresh(&pool, state.admin_refresh_ttl, id).await;
    sqlx::query("UPDATE admin_refresh_tokens SET rotated_at = NOW() - INTERVAL '30 seconds' WHERE admin_id = $1 AND rotated_at IS NOT NULL")
        .bind(id).execute(&pool).await.unwrap();
    assert_eq!(
        refresh(&state, Some(&old.plaintext)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        refresh(&state, Some(&current.refresh.plaintext)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        refresh(&state, Some(&other.plaintext)).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn legacy_tab_recovery_rejects_consumed_cookie_without_revoking_current_sessions(
    pool: PgPool,
) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool).await;
    let old = issue_refresh(&pool, state.admin_refresh_ttl, id).await;
    let svc = AdminSessionService::new(
        AdminRefreshTokenRepository::new(pool.clone()),
        state.admin_refresh_ttl,
    );
    let current = svc.rotate(&old.plaintext).await.unwrap();
    sqlx::query("UPDATE admin_refresh_tokens SET rotated_at=rotated_at-interval '1 hour' WHERE admin_id=$1 AND rotated_at IS NOT NULL")
        .bind(id).execute(&pool).await.unwrap();
    let key = Uuid::parse_str("283e8512-4e3c-4c59-a02b-2f383a97e9ae").unwrap();
    let rejected = refresh_with_mode(&state, Some(&old.plaintext), Some(key), true).await;
    assert_eq!(rejected.0, StatusCode::UNAUTHORIZED);
    assert!(rejected.1.is_none());
    let live: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_refresh_tokens WHERE admin_id=$1 AND revoked_at IS NULL AND rotated_at IS NULL")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(live, 1, "旧标记恢复不能连坐撤销当前会话");
    let restored =
        refresh_with_mode(&state, Some(&current.refresh.plaintext), Some(key), true).await;
    assert_eq!(restored.0, StatusCode::OK, "有效 Cookie 应可直接恢复");
    assert_eq!(
        refresh_with_mode(&state, Some(&current.refresh.plaintext), Some(key), true)
            .await
            .1,
        restored.1,
        "恢复响应丢失仍须同键续接"
    );
}
