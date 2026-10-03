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
    .issue(&admin_id, 0)
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

#[sqlx::test]
async fn own_password_change_revokes_access_refresh_and_preserves_raw_password(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let old = " Original!密码 river cloud ";
    let new = " New!密码🙂 silver orchard ";
    let hash = Password::parse(old).unwrap().hash().await.unwrap();
    let id = seed_admin(&pool, AdminRole::Admin, &hash, false).await;
    let old_access = token(&state, id, AdminRole::Admin);
    let session = AdminSessionService::new(
        AdminRefreshTokenRepository::new(pool.clone()),
        Duration::days(7),
    );
    let old_refresh = session.issue(&id, 0).await.unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/auth/change-password")
                .header(header::AUTHORIZATION, format!("Bearer {old_access}"))
                .header(
                    header::COOKIE,
                    format!("admin_refresh_token={}", old_refresh.plaintext),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"current_password":old,"new_password":new}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(active_session_count(&pool, id).await, 0);
    assert!(session.rotate(&old_refresh.plaintext).await.is_err());
    let after = AdminRepository::new(pool.clone())
        .get_by_id(&id)
        .await
        .unwrap();
    assert_eq!(after.security_version, 1);
    assert!(Password::verify_raw(new.to_owned(), after.password_hash.clone()).await);
    assert!(!Password::verify_raw(new.to_uppercase(), after.password_hash.clone()).await);
    for (access, expected) in [
        (old_access, StatusCode::UNAUTHORIZED),
        (
            state
                .admin_token_manager
                .generate_with_version(id, AdminRole::Admin.as_str(), after.security_version)
                .unwrap(),
            StatusCode::OK,
        ),
    ] {
        let response = tsz_rust::router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/admin/profile")
                    .header(header::AUTHORIZATION, format!("Bearer {access}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    // 旧密码认证后晚到的登录签发，不能在改密后建立新 refresh。
    assert!(session.issue(&id, 0).await.is_err());
    assert_eq!(active_session_count(&pool, id).await, 0);
}

#[sqlx::test]
async fn password_and_session_revocation_roll_back_together(pool: PgPool) {
    let id = seed_admin(&pool, AdminRole::Admin, "old-hash", false).await;
    issue_session(&pool, id).await;
    sqlx::raw_sql("CREATE FUNCTION reject_password_revocation() RETURNS trigger AS $$ BEGIN RAISE EXCEPTION 'test revocation failure'; END; $$ LANGUAGE plpgsql; CREATE TRIGGER reject_password_revocation BEFORE UPDATE ON admin_refresh_tokens FOR EACH ROW EXECUTE FUNCTION reject_password_revocation();")
        .execute(&pool).await.unwrap();
    assert!(
        AdminRepository::new(pool.clone())
            .set_password(&id, "new-hash", false)
            .await
            .is_err()
    );
    let after = AdminRepository::new(pool.clone())
        .get_by_id(&id)
        .await
        .unwrap();
    assert_eq!(after.password_hash, "old-hash");
    assert_eq!(after.security_version, 0);
    assert_eq!(active_session_count(&pool, id).await, 1);
}

#[sqlx::test]
async fn refresh_waits_for_password_change_and_cannot_escape_revocation(pool: PgPool) {
    let id = seed_admin(&pool, AdminRole::Admin, "old-hash", false).await;
    let session = AdminSessionService::new(
        AdminRefreshTokenRepository::new(pool.clone()),
        Duration::days(7),
    );
    let old = session.issue(&id, 0).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE admins SET security_version = security_version + 1 WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let rotate = tokio::spawn(async move { session.rotate(&old.plaintext).await });
    // 使用第二连接的实际锁等待证据，避免以睡眠推断轮换到达锁点。
    let mut observed_wait = false;
    for _ in 0..100 {
        let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE '%FOR UPDATE OF a%')").fetch_one(&pool).await.unwrap();
        if waiting {
            observed_wait = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(observed_wait, "轮换必须实际进入管理员行锁等待");
    sqlx::query("UPDATE admin_refresh_tokens SET revoked_at = NOW() WHERE admin_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(rotate.await.unwrap().is_err());
    assert_eq!(active_session_count(&pool, id).await, 0);
}

#[sqlx::test]
async fn admin_security_version_cannot_be_removed_after_password_change(pool: PgPool) {
    let id = seed_admin(&pool, AdminRole::Admin, "old-hash", false).await;
    AdminRepository::new(pool.clone())
        .set_password(&id, "new-hash", false)
        .await
        .unwrap();
    let current_version: i64 =
        sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success IS TRUE")
            .fetch_one(&pool)
            .await
            .unwrap();
    let error = tsz_rust::deployment_migrations::undo(&pool, 20260928010000, current_version)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("cannot remove admin security_version"));
    assert_eq!(
        AdminRepository::new(pool)
            .get_by_id(&id)
            .await
            .unwrap()
            .security_version,
        1
    );
}
