//! `PATCH /api/v1/admin/admins/{admin_id}/status` 的治理契约（设计 §9 矩阵）。
//!
//! 这条路由此前挂着一个空壳 handler（函数体 `Ok(())`），前端调用拿到 200 却什么都
//! 没发生。所以这里的每条正向断言都**回库比对**，只看响应体不足以证明副作用发生了。

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{AdminRepository, AdminRole, AdminStatus, NewAdmin},
    state::AppState,
};

async fn seed_admin(pool: &PgPool, role: AdminRole, must_change_password: bool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.as_u128().to_string(),
            display_name: "测试管理员".to_owned(),
            password_hash: "hashed-pw".to_owned(),
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

async fn patch_status(
    state: &AppState,
    target: Uuid,
    bearer: Option<&str>,
    body: Value,
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method("PATCH")
        .uri(format!("/api/v1/admin/admins/{target}/status"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }

    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn stored_status(pool: &PgPool, id: Uuid) -> AdminStatus {
    AdminRepository::new(pool.clone())
        .get_by_id(&id)
        .await
        .expect("目标管理员应存在")
        .status
}

#[sqlx::test]
async fn super_admin_disables_plain_admin_and_change_is_persisted(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let target = seed_admin(&pool, AdminRole::Admin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, body) = patch_status(
        &state,
        target,
        Some(&bearer),
        json!({ "status": "disabled" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["id"], target.to_string());
    assert_eq!(response["status"], "disabled");
    assert_eq!(response["role"], "admin");
    // 防泄惯例：手挑字段，凭证与锁定列绝不出现在 wire 上。
    assert!(response.get("password_hash").is_none());
    assert!(response.get("must_change_password").is_none());
    assert!(response.get("locked_until").is_none());
    assert_eq!(
        stored_status(&pool, target).await,
        AdminStatus::Disabled,
        "200 必须对应真实落库，空壳成功是本端点修复前的病根"
    );
}

#[sqlx::test]
async fn super_admin_reenables_disabled_admin(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let target = seed_admin(&pool, AdminRole::Admin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    patch_status(
        &state,
        target,
        Some(&bearer),
        json!({ "status": "disabled" }),
    )
    .await;
    let (status, body) =
        patch_status(&state, target, Some(&bearer), json!({ "status": "active" })).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stored_status(&pool, target).await, AdminStatus::Active);
}

#[sqlx::test]
async fn response_carries_creator_like_the_list_does(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let target_id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id: target_id,
            phone: target_id.as_u128().to_string(),
            display_name: "被创建的管理员".to_owned(),
            password_hash: "hashed-pw".to_owned(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: Some(actor),
        })
        .await
        .expect("seed admin 应成功");
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, body) = patch_status(
        &state,
        target_id,
        Some(&bearer),
        json!({ "status": "disabled" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    // 与列表同形状：前端可以直接用响应替换列表里的那一行，不会把「创建者」列刷空。
    assert_eq!(response["created_by"]["id"], actor.to_string());
    assert_eq!(response["created_by"]["display_name"], "测试管理员");
}

#[sqlx::test]
async fn super_admin_target_is_rejected(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let target = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, body) = patch_status(
        &state,
        target,
        Some(&bearer),
        json!({ "status": "disabled" }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        stored_status(&pool, target).await,
        AdminStatus::Active,
        "治理顶点互不可管：拒绝必须是零副作用的"
    );
}

#[sqlx::test]
async fn super_admin_cannot_disable_self(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, _) = patch_status(
        &state,
        actor,
        Some(&bearer),
        json!({ "status": "disabled" }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(stored_status(&pool, actor).await, AdminStatus::Active);
}

#[sqlx::test]
async fn plain_admin_cannot_change_status(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::Admin, false).await;
    let target = seed_admin(&pool, AdminRole::Admin, false).await;
    let bearer = token(&state, actor, AdminRole::Admin);

    let (status, _) = patch_status(
        &state,
        target,
        Some(&bearer),
        json!({ "status": "disabled" }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(stored_status(&pool, target).await, AdminStatus::Active);
}

#[sqlx::test]
async fn actor_pending_password_change_is_rejected(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, true).await;
    let target = seed_admin(&pool, AdminRole::Admin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, body) = patch_status(
        &state,
        target,
        Some(&bearer),
        json!({ "status": "disabled" }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let problem: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(problem["code"], "must_change_password");
}

#[sqlx::test]
async fn unknown_target_is_not_found(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, body) = patch_status(
        &state,
        Uuid::now_v7(),
        Some(&bearer),
        json!({ "status": "disabled" }),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let problem: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(problem["code"], "not_found");
}

#[sqlx::test]
async fn status_outside_enum_is_rejected(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let target = seed_admin(&pool, AdminRole::Admin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);

    let (status, body) = patch_status(
        &state,
        target,
        Some(&bearer),
        json!({ "status": "deleted" }),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(stored_status(&pool, target).await, AdminStatus::Active);
}

#[sqlx::test]
async fn anonymous_request_is_unauthorized(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let target = seed_admin(&pool, AdminRole::Admin, false).await;

    let (status, _) = patch_status(&state, target, None, json!({ "status": "disabled" })).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(stored_status(&pool, target).await, AdminStatus::Active);
}
