//! `POST /api/v1/admin/admins/{admin_id}/reset-password` 的治理契约（设计 §9 + hardening-D5）。
//!
//! 这条路由此前挂着空壳 handler（函数体 `Ok(())`），200 却零副作用。所以正向用例
//! 一律回库比对三件事：新哈希能被返回的明文验过、`must_change_password` 被置起、
//! 目标的活跃会话全被吊销。

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::Duration;
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{
        AdminRefreshTokenRepository, AdminRepository, AdminRole, AdminSessionService, NewAdmin,
    },
    platform::Password,
    state::AppState,
};

async fn seed_admin(
    pool: &PgPool,
    role: AdminRole,
    password_hash: &str,
    must_change_password: bool,
) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.as_u128().to_string(),
            display_name: "测试管理员".to_owned(),
            password_hash: password_hash.to_owned(),
            role,
            must_change_password,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin 应成功");
    id
}

fn token(state: &AppState, id: Uuid, role: AdminRole) -> String {
    state
        .admin_token_manager
        .generate(id, role.as_str())
        .expect("签 admin token 应成功")
}

async fn issue_session(pool: &PgPool, admin_id: Uuid) {
    AdminSessionService::new(
        AdminRefreshTokenRepository::new(pool.clone()),
        Duration::days(7),
    )
    .issue(&admin_id)
    .await
    .expect("签发测试 refresh 应成功");
}

async fn active_session_count(pool: &PgPool, admin_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM admin_refresh_tokens WHERE admin_id = $1 AND revoked_at IS NULL",
    )
    .bind(admin_id)
    .fetch_one(pool)
    .await
    .expect("统计活跃会话应成功")
}

async fn reset(state: &AppState, target: Uuid, bearer: Option<&str>) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/admin/admins/{target}/reset-password"));
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }

    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[sqlx::test]
async fn reset_returns_working_temporary_password_and_forces_change(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, "hashed-pw", false).await;
    let target = seed_admin(&pool, AdminRole::Admin, "old-hash", false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, body) = reset(&state, target, Some(&bearer)).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    let temporary_password = response["temporary_password"]
        .as_str()
        .expect("响应应带明文临时密码")
        .to_owned();
    assert!(!temporary_password.is_empty());

    let stored = AdminRepository::new(pool.clone())
        .get_by_id(&target)
        .await
        .expect("目标管理员应存在");
    assert_ne!(stored.password_hash, "old-hash", "旧哈希必须被覆盖");
    assert!(
        Password::verify_raw(temporary_password, stored.password_hash.clone()).await,
        "响应里的明文必须能验过落库的新哈希"
    );
    assert!(
        stored.must_change_password,
        "重置出来的临时密码必须逼目标下次登录先改密（设计 §7）"
    );
}

#[sqlx::test]
async fn reset_revokes_every_session_of_the_target(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, "hashed-pw", false).await;
    let target = seed_admin(&pool, AdminRole::Admin, "old-hash", false).await;
    let bystander = seed_admin(&pool, AdminRole::Admin, "old-hash", false).await;
    issue_session(&pool, target).await;
    issue_session(&pool, bystander).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, body) = reset(&state, target, Some(&bearer)).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        active_session_count(&pool, target).await,
        0,
        "重置必须先踢掉目标的全部会话（hardening-D5）"
    );
    assert_eq!(
        active_session_count(&pool, bystander).await,
        1,
        "不得殃及其他管理员的会话"
    );
}

#[sqlx::test]
async fn reset_is_repeatable_and_yields_a_new_password_each_time(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, "hashed-pw", false).await;
    let target = seed_admin(&pool, AdminRole::Admin, "old-hash", false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (_, first) = reset(&state, target, Some(&bearer)).await;
    let (status, second) = reset(&state, target, Some(&bearer)).await;

    assert_eq!(status, StatusCode::OK, "{second}");
    let first: Value = serde_json::from_str(&first).unwrap();
    let second: Value = serde_json::from_str(&second).unwrap();
    assert_ne!(
        first["temporary_password"], second["temporary_password"],
        "每次重置都应是全新的临时密码"
    );
}

#[sqlx::test]
async fn super_admin_target_is_rejected(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, "hashed-pw", false).await;
    let target = seed_admin(&pool, AdminRole::SuperAdmin, "peer-hash", false).await;
    issue_session(&pool, target).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, body) = reset(&state, target, Some(&bearer)).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let stored = AdminRepository::new(pool.clone())
        .get_by_id(&target)
        .await
        .expect("目标管理员应存在");
    assert_eq!(stored.password_hash, "peer-hash", "拒绝必须零副作用");
    assert_eq!(active_session_count(&pool, target).await, 1);
}

#[sqlx::test]
async fn super_admin_cannot_reset_self(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, "own-hash", false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, _) = reset(&state, actor, Some(&bearer)).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    let stored = AdminRepository::new(pool.clone())
        .get_by_id(&actor)
        .await
        .expect("超管应存在");
    assert_eq!(stored.password_hash, "own-hash");
}

#[sqlx::test]
async fn plain_admin_cannot_reset_anyone(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::Admin, "hashed-pw", false).await;
    let target = seed_admin(&pool, AdminRole::Admin, "old-hash", false).await;
    let bearer = token(&state, actor, AdminRole::Admin);

    let (status, _) = reset(&state, target, Some(&bearer)).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    let stored = AdminRepository::new(pool.clone())
        .get_by_id(&target)
        .await
        .expect("目标管理员应存在");
    assert_eq!(stored.password_hash, "old-hash");
}

#[sqlx::test]
async fn unknown_target_is_not_found(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, "hashed-pw", false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, body) = reset(&state, Uuid::now_v7(), Some(&bearer)).await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let problem: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(problem["code"], "not_found");
}

#[sqlx::test]
async fn anonymous_request_is_unauthorized(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let target = seed_admin(&pool, AdminRole::Admin, "old-hash", false).await;

    let (status, _) = reset(&state, target, None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let stored = AdminRepository::new(pool.clone())
        .get_by_id(&target)
        .await
        .expect("目标管理员应存在");
    assert_eq!(stored.password_hash, "old-hash");
}
