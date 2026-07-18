//! `AuthUser` 提取器的契约测试（直接调 `from_request_parts`，不经 /me 路由——路由还没接）。
//!
//! 钉死的契约：
//! - 合法 `Bearer <jwt>` → `Ok(AuthUser)`，且 subject/role 与签发时一致
//! - 缺头 / 错 scheme / 乱码 / 过期 / 过短的头 → **401**（且**绝不 panic**）
//!
//! 提取器只碰 `token_manager.parse`，不碰 PG/Redis；用 `#[sqlx::test]` 只是为了拿 pool 造 AppState。

use axum::extract::FromRequestParts;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use chrono::Duration;
use sqlx::PgPool;
use uuid::Uuid;

use tsz_rust::auth::extract::AuthUser;
use tsz_rust::auth::{Realm, TokenManager};
use tsz_rust::state::AppState;

/// 用给定 Authorization 头值造 Parts（None = 不带该头）。
fn parts_with_auth(value: Option<&str>) -> axum::http::request::Parts {
    let mut builder = Request::builder().method("GET").uri("/me");
    if let Some(v) = value {
        builder = builder.header("Authorization", v);
    }
    builder.body(()).unwrap().into_parts().0
}

/// 跑提取器，成功→200、失败→其真实状态码。避免对 `AuthUser` 要求 `Debug`。
async fn status(state: &AppState, value: Option<&str>) -> StatusCode {
    let mut parts = parts_with_auth(value);
    match AuthUser::from_request_parts(&mut parts, state).await {
        Ok(_) => StatusCode::OK,
        Err(e) => e.into_response().status(),
    }
}

#[sqlx::test]
async fn valid_bearer_token_yields_authuser(pool: PgPool) {
    let state = AppState::for_test(pool);
    let subject = Uuid::now_v7();
    // 用 state 自己的 token_manager 签，才能被同一 manager parse 通过。
    let token = state.token_manager.generate(subject, "student").unwrap();

    let mut parts = parts_with_auth(Some(&format!("Bearer {token}")));
    let au = AuthUser::from_request_parts(&mut parts, &state)
        .await
        .expect("合法 Bearer token 应被提取为 AuthUser（而非 401）");
    assert_eq!(
        au.subject, subject,
        "提取出的 subject 应等于签发时的 subject"
    );
    assert_eq!(au.role, "student");
}

#[sqlx::test]
async fn lowercase_scheme_is_accepted(pool: PgPool) {
    // 决策：scheme 大小写不敏感（RFC 7235）。`bearer`/`BEARER` 与 `Bearer` 同等有效。
    // 钉死这条，防止有人把 eq_ignore_ascii_case 改回严格 `!=`。
    let state = AppState::for_test(pool);
    let token = state
        .token_manager
        .generate(Uuid::now_v7(), "student")
        .unwrap();
    assert_eq!(
        status(&state, Some(&format!("bearer {token}"))).await,
        StatusCode::OK,
        "小写 scheme `bearer` 应被接受"
    );
}

#[sqlx::test]
async fn missing_header_is_401(pool: PgPool) {
    let state = AppState::for_test(pool);
    assert_eq!(status(&state, None).await, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn wrong_scheme_is_401(pool: PgPool) {
    let state = AppState::for_test(pool);
    assert_eq!(
        status(&state, Some("Basic dXNlcjpwYXNz")).await,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn garbage_token_is_401(pool: PgPool) {
    let state = AppState::for_test(pool);
    assert_eq!(
        status(&state, Some("Bearer not-a-real-jwt")).await,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn expired_token_is_401(pool: PgPool) {
    let state = AppState::for_test(pool);
    // 相同 secret+realm、负 TTL → “出生即过期”。secret/realm 要和 AppState::for_test 里一致。
    let expired_mgr = TokenManager::new("test-secret", Realm::Web, Duration::seconds(-3600));
    let token = expired_mgr.generate(Uuid::now_v7(), "student").unwrap();
    assert_eq!(
        status(&state, Some(&format!("Bearer {token}"))).await,
        StatusCode::UNAUTHORIZED,
        "过期 token 应 401"
    );
}

#[sqlx::test]
async fn short_header_does_not_panic(pool: PgPool) {
    let state = AppState::for_test(pool);
    // 比 scheme 还短的头值：绝不能 panic，应老实 401。
    assert_eq!(
        status(&state, Some("abc")).await,
        StatusCode::UNAUTHORIZED,
        "过短的头应 401 而非 panic"
    );
}
