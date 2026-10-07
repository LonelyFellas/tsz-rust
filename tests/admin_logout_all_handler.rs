//! `POST /api/v1/admin/auth/logout-all` 的契约（设计 §7 逃生组 + §11）。
//!
//! 这条端点一期就在契约里，但从未落地。两条硬契约：
//!   - 吊销**该管理员的全部** access / refresh，旧 Bearer 重试返回 401；
//!   - 属逃生组：`must_change_password` 的管理员必须能调通——否则被强制改密者
//!     除了改密之外无路可走。

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::Duration;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{
        ADMIN_REFRESH_TOKEN_COOKIE, AdminRefreshTokenRepository, AdminRepository, AdminRole,
        AdminSessionService, NewAdmin,
    },
    state::AppState,
};

async fn seed_admin(pool: &PgPool, must_change_password: bool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.as_u128().to_string(),
            display_name: "测试管理员".to_owned(),
            password_hash: "hashed-pw".to_owned(),
            role: AdminRole::Admin,
            must_change_password,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin 应成功");
    id
}

fn token(state: &AppState, id: Uuid) -> String {
    state
        .admin_token_manager
        .generate(id, AdminRole::Admin.as_str())
        .expect("签 admin token 应成功")
}

fn session_service(pool: &PgPool) -> AdminSessionService {
    AdminSessionService::new(
        AdminRefreshTokenRepository::new(pool.clone()),
        Duration::days(7),
    )
}

/// 直接插行造多枚活跃会话。
async fn insert_active_session(pool: &PgPool, admin_id: Uuid) {
    sqlx::query(
        r#"
        INSERT INTO admin_refresh_tokens (id, admin_id, token_hash, expires_at)
        VALUES ($1, $2, $3, NOW() + INTERVAL '7 days')
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(admin_id)
    .bind(Uuid::now_v7().to_string())
    .execute(pool)
    .await
    .expect("插入测试会话应成功");
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

async fn logout_all(
    state: &AppState,
    bearer: Option<&str>,
    cookie: Option<&str>,
) -> (StatusCode, Vec<String>) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/auth/logout-all");
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    if let Some(cookie) = cookie {
        builder = builder.header(
            header::COOKIE,
            format!("{ADMIN_REFRESH_TOKEN_COOKIE}={cookie}"),
        );
    }

    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().to_owned())
        .collect();
    (status, cookies)
}

#[sqlx::test]
async fn logout_all_revokes_every_session_of_the_caller(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, false).await;
    let bystander = seed_admin(&pool, false).await;
    insert_active_session(&pool, admin).await;
    insert_active_session(&pool, admin).await;
    insert_active_session(&pool, bystander).await;
    let bearer = token(&state, admin);

    let (status, _) = logout_all(&state, Some(&bearer), None).await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(active_session_count(&pool, admin).await, 0);
    assert_eq!(
        active_session_count(&pool, bystander).await,
        1,
        "只吊销调用者自己的会话"
    );
}

#[sqlx::test]
async fn logout_all_kills_the_caller_own_refresh_and_clears_the_cookie(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, false).await;
    let issued = session_service(&pool)
        .issue(&admin, 0)
        .await
        .expect("签发 refresh 应成功");
    let bearer = token(&state, admin);

    let (status, cookies) = logout_all(&state, Some(&bearer), Some(&issued.plaintext)).await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    // 本次请求带的那枚也在吊销范围内，不能留一枚“看着还在、用起来 401”的 cookie。
    assert!(
        session_service(&pool)
            .rotate(&issued.plaintext)
            .await
            .map(drop)
            .is_err(),
        "自己那枚 refresh 也必须失效"
    );
    let cleared = cookies
        .iter()
        .find(|cookie| cookie.starts_with(ADMIN_REFRESH_TOKEN_COOKIE))
        .expect("应下发清除 cookie");
    assert!(cleared.contains("Path=/api/v1/admin/auth"));
    assert!(cleared.contains("Max-Age=0"));
}

#[sqlx::test]
async fn admin_pending_password_change_can_still_logout_all(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, true).await;
    insert_active_session(&pool, admin).await;
    let bearer = token(&state, admin);

    let (status, _) = logout_all(&state, Some(&bearer), None).await;

    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "逃生组端点不过 must_change_password 守卫"
    );
    assert_eq!(active_session_count(&pool, admin).await, 0);
}

#[sqlx::test]
async fn logout_all_invalidates_bearer_even_without_refresh_sessions(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, false).await;
    let bearer = token(&state, admin);

    let (first, _) = logout_all(&state, Some(&bearer), None).await;
    let (second, _) = logout_all(&state, Some(&bearer), None).await;

    assert_eq!(first, StatusCode::NO_CONTENT);
    assert_eq!(second, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn anonymous_request_is_unauthorized(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, false).await;
    insert_active_session(&pool, admin).await;

    let (status, _) = logout_all(&state, None, None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        active_session_count(&pool, admin).await,
        1,
        "无凭证的请求不得产生任何副作用"
    );
}

#[sqlx::test]
async fn web_realm_token_cannot_logout_admin_sessions(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, false).await;
    insert_active_session(&pool, admin).await;
    // 跨 realm 隔离：C 端 token 的 aud 不是 admin，提取器必须拒。
    let web_token = state
        .token_manager
        .generate(admin, "student")
        .expect("签 web token 应成功");

    let (status, _) = logout_all(&state, Some(&web_token), None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(active_session_count(&pool, admin).await, 1);
}

async fn profile_status(state: &AppState, bearer: &str) -> StatusCode {
    tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/profile")
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[sqlx::test]
async fn logout_all_invalidates_old_credentials_and_stale_retry_preserves_new_login(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, false).await;
    let bystander = seed_admin(&pool, false).await;
    let svc = session_service(&pool);
    let first = svc.issue(&admin, 0).await.unwrap();
    let second = svc.issue(&admin, 0).await.unwrap();
    let unaffected = svc.issue(&bystander, 0).await.unwrap();
    let old_access = token(&state, admin);
    assert_eq!(
        logout_all(&state, Some(&old_access), None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        profile_status(&state, &old_access).await,
        StatusCode::UNAUTHORIZED
    );
    assert!(svc.rotate(&first.plaintext).await.is_err());
    assert!(svc.rotate(&second.plaintext).await.is_err());
    svc.rotate(&unaffected.plaintext).await.unwrap();
    assert_eq!(
        profile_status(&state, &token(&state, bystander)).await,
        StatusCode::OK
    );
    let new_refresh = svc.issue(&admin, 1).await.unwrap();
    let new_access = state
        .admin_token_manager
        .generate_with_version(admin, AdminRole::Admin.as_str(), 1)
        .unwrap();
    assert_eq!(
        logout_all(&state, Some(&old_access), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    // 模拟已通过 extractor、随后迟到的请求，同样须由事务的版本比较拒绝。
    assert!(svc.logout_all(&admin, 0).await.is_err());
    assert_eq!(profile_status(&state, &new_access).await, StatusCode::OK);
    svc.rotate(&new_refresh.plaintext).await.unwrap();
}

#[sqlx::test]
async fn logout_all_failure_rolls_back_version_and_keeps_credentials(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, false).await;
    let svc = session_service(&pool);
    let first = svc.issue(&admin, 0).await.unwrap();
    let second = svc.issue(&admin, 0).await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_revocation() RETURNS trigger AS $$ BEGIN RAISE EXCEPTION 'test revocation failure'; END; $$ LANGUAGE plpgsql; CREATE TRIGGER reject_revocation BEFORE UPDATE ON admin_refresh_tokens FOR EACH ROW EXECUTE FUNCTION reject_revocation();")
        .execute(&pool).await.unwrap();
    let bearer = token(&state, admin);
    let (status, cookies) = logout_all(&state, Some(&bearer), None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(cookies.is_empty());
    assert_eq!(profile_status(&state, &bearer).await, StatusCode::OK);
    sqlx::query("DROP TRIGGER reject_revocation ON admin_refresh_tokens")
        .execute(&pool)
        .await
        .unwrap();
    svc.rotate(&first.plaintext).await.unwrap();
    svc.rotate(&second.plaintext).await.unwrap();
}
