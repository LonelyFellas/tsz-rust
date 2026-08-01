//! 创建管理员确认码与创建端点的端到端契约。

use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::{
    admin::{AdminRepository, AdminRole, AdminService, NewAdmin},
    otp::model::Purpose,
    state::AppState,
};
use uuid::Uuid;

const SUPER_PHONE: &str = "13800138000";
const NEW_ADMIN_PHONE: &str = "13800138001";
const CODE: &str = "123456";

async fn seed_super_admin(pool: &PgPool, phone: &str) -> Uuid {
    let outcome = AdminService::for_seed(AdminRepository::new(pool.clone()))
        .seed_super_admin(phone, "password123", "测试超管")
        .await
        .expect("seed 超管应成功");
    match outcome {
        tsz_rust::admin::SeedOutcome::Created(admin)
        | tsz_rust::admin::SeedOutcome::Unchanged(admin) => admin.id,
    }
}

async fn seed_plain_admin(pool: &PgPool, phone: &str) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: phone.into(),
            display_name: "普通管理员".into(),
            password_hash: "unused-test-hash".into(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .expect("seed 普通管理员应成功");
    id
}

fn token(state: &AppState, id: Uuid, role: AdminRole) -> String {
    state
        .admin_token_manager
        .generate(id, role.as_str())
        .expect("签发测试 token 应成功")
}

async fn post(
    state: &AppState,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, String) {
    let mut request = Request::builder().method("POST").uri(path);
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let body = match body {
        Some(value) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = tsz_rust::router(state.clone())
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[sqlx::test]
async fn super_admin_can_request_create_code_for_own_phone(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let id = seed_super_admin(&pool, SUPER_PHONE).await;
    let bearer = token(&state, id, AdminRole::SuperAdmin);

    let (status, body) = post(
        &state,
        "/api/v1/admin/admins/create-code",
        Some(&bearer),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(body.is_empty());
    assert!(
        store
            .code_exists(SUPER_PHONE, Purpose::AdminCreate)
            .await
            .unwrap()
    );
}

#[sqlx::test]
async fn plain_admin_cannot_request_create_code(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let phone = "13800138009";
    let id = seed_plain_admin(&pool, phone).await;
    let bearer = token(&state, id, AdminRole::Admin);

    let (status, _) = post(
        &state,
        "/api/v1/admin/admins/create-code",
        Some(&bearer),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        !store
            .code_exists(phone, Purpose::AdminCreate)
            .await
            .unwrap()
    );
}

#[sqlx::test]
async fn create_verifies_code_against_calling_super_admin(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let id = seed_super_admin(&pool, SUPER_PHONE).await;
    let bearer = token(&state, id, AdminRole::SuperAdmin);
    store
        .save_code(
            SUPER_PHONE,
            Purpose::AdminCreate,
            CODE,
            Duration::from_secs(300),
        )
        .await
        .unwrap();

    let (status, body) = post(
        &state,
        "/api/v1/admin/admins",
        Some(&bearer),
        Some(json!({
            "phone": NEW_ADMIN_PHONE,
            "display_name": "新管理员",
            "code": CODE
        })),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["admin"]["phone"], NEW_ADMIN_PHONE);
    assert_eq!(response["admin"]["created_by"]["id"], id.to_string());
    assert!(
        response["temporary_password"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        !store
            .code_exists(SUPER_PHONE, Purpose::AdminCreate)
            .await
            .unwrap(),
        "成功使用后验证码必须失效"
    );
}

#[sqlx::test]
async fn login_code_cannot_authorize_admin_creation(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let id = seed_super_admin(&pool, SUPER_PHONE).await;
    let bearer = token(&state, id, AdminRole::SuperAdmin);
    store
        .save_code(
            SUPER_PHONE,
            Purpose::AdminLogin,
            CODE,
            Duration::from_secs(300),
        )
        .await
        .unwrap();

    let (status, _) = post(
        &state,
        "/api/v1/admin/admins",
        Some(&bearer),
        Some(json!({ "phone": NEW_ADMIN_PHONE, "code": CODE })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        store
            .code_exists(SUPER_PHONE, Purpose::AdminLogin)
            .await
            .unwrap(),
        "其他 purpose 的验证码不应被消费"
    );
}
