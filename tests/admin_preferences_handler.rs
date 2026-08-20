//! `PATCH /api/v1/admin/profile/preferences`（管理员个人偏好）端到端契约。
//!
//! 英美方言偏好化 A1 · 后端提案 P2：把「英美」从每条词条都要做的决策，降级为
//! 管理员账号上设置一次的偏好。本文件钉死的编排契约：
//!
//! ```text
//! ① 缺 token                        ⇒ 401（提取器层）
//! ② disabled / must_change_password ⇒ 403（守卫组，与 profile 同一组）
//! ③ dialect 不在枚举内或缺字段      ⇒ 422 invalid_request_body（problem+json）
//! ④ 成功 ⇒ 200 {preferences:{dialect}}，且 GET /profile 立刻读到同一个值
//! ⑤ 改的恒是 token subject 自己：请求体里没有管理员 ID，改不到别人
//! ```

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::admin::{AdminRepository, AdminRole, NewAdmin};
use tsz_rust::state::AppState;

async fn seed_admin(pool: &PgPool, must_change: bool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.to_string(),
            display_name: "测试管理员".to_owned(),
            password_hash: "hashed-pw".to_owned(),
            role: AdminRole::Admin,
            must_change_password: must_change,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin 应成功");
    id
}

fn admin_token(state: &AppState, id: Uuid) -> String {
    state
        .admin_token_manager
        .generate(id, AdminRole::Admin.as_str())
        .expect("签 admin token 应成功")
}

async fn call(
    state: &AppState,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = tsz_rust::router(state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn patch_preferences(
    state: &AppState,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call(
        state,
        "PATCH",
        "/api/v1/admin/profile/preferences",
        bearer,
        body,
    )
    .await
}

async fn profile_dialect(state: &AppState, bearer: &str) -> String {
    let (status, body) = call(state, "GET", "/api/v1/admin/profile", Some(bearer), None).await;
    assert_eq!(status, StatusCode::OK, "profile 应 200：{body}");
    body["preferences"]["dialect"]
        .as_str()
        .expect("profile 应恒带 preferences.dialect")
        .to_owned()
}

#[sqlx::test]
async fn preference_write_is_readable_from_profile_and_idempotent(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, false).await;
    let bearer = admin_token(&state, id);

    assert_eq!(profile_dialect(&state, &bearer).await, "uk", "默认应为英式");

    let (status, body) =
        patch_preferences(&state, Some(&bearer), Some(json!({"dialect": "us"}))).await;
    assert_eq!(status, StatusCode::OK, "改偏好应 200：{body}");
    assert_eq!(body["preferences"]["dialect"], "us");
    assert_eq!(
        profile_dialect(&state, &bearer).await,
        "us",
        "profile 应立刻读到落库后的值"
    );

    // 同值重复写是幂等的：前端「点了两次英式」不该出错。
    for _ in 0..2 {
        let (status, body) =
            patch_preferences(&state, Some(&bearer), Some(json!({"dialect": "uk"}))).await;
        assert_eq!(status, StatusCode::OK, "改回英式应 200：{body}");
        assert_eq!(body["preferences"]["dialect"], "uk");
    }
    assert_eq!(profile_dialect(&state, &bearer).await, "uk");
}

#[sqlx::test]
async fn preference_write_only_touches_the_token_subject(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let mine = seed_admin(&pool, false).await;
    let other = seed_admin(&pool, false).await;
    let my_bearer = admin_token(&state, mine);
    let other_bearer = admin_token(&state, other);

    let (status, _) =
        patch_preferences(&state, Some(&my_bearer), Some(json!({"dialect": "us"}))).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(profile_dialect(&state, &my_bearer).await, "us");
    assert_eq!(
        profile_dialect(&state, &other_bearer).await,
        "uk",
        "偏好是账号级设置，改自己的不该动到别人"
    );
}

#[sqlx::test]
async fn invalid_dialect_and_missing_field_are_422_problems(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, false).await;
    let bearer = admin_token(&state, id);

    for body in [json!({"dialect": "au"}), json!({"dialect": ""}), json!({})] {
        let (status, problem) = patch_preferences(&state, Some(&bearer), Some(body.clone())).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body} 应被拒：{problem}"
        );
        assert_eq!(problem["code"], "invalid_request_body");
        assert_eq!(problem["status"], 422);
    }
    assert_eq!(
        profile_dialect(&state, &bearer).await,
        "uk",
        "被拒的请求不应改动落库值"
    );
}

#[sqlx::test]
async fn missing_token_is_401(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let (status, _) = patch_preferences(&state, None, Some(json!({"dialect": "us"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn disabled_or_must_change_password_admin_is_403(pool: PgPool) {
    let state = AppState::for_test(pool.clone());

    let disabled = seed_admin(&pool, false).await;
    sqlx::query("UPDATE admins SET status = 'disabled' WHERE id = $1")
        .bind(disabled)
        .execute(&pool)
        .await
        .expect("禁用管理员应成功");
    let (status, problem) = patch_preferences(
        &state,
        Some(&admin_token(&state, disabled)),
        Some(json!({"dialect": "us"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert_eq!(problem["code"], "account_disabled");

    let must_change = seed_admin(&pool, true).await;
    let (status, problem) = patch_preferences(
        &state,
        Some(&admin_token(&state, must_change)),
        Some(json!({"dialect": "us"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert_eq!(problem["code"], "must_change_password");
}
