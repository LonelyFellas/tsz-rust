//! `GET /api/v1/admin/users` 的路由与列表契约测试。

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    state::AppState,
};

async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: "13800138000".to_owned(),
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

async fn seed_user(pool: &PgPool, display_name: &str, roles: &[&str]) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO users (id, phone, password_hash, display_name, avatar_url)
        VALUES ($1, $2, 'hashed-pw', $3, '')
        "#,
    )
    .bind(id)
    .bind(format!("{}", id.as_u128()))
    .bind(display_name)
    .execute(pool)
    .await
    .expect("seed user 应成功");

    seed_user_roles(pool, id, roles).await;
    id
}

async fn seed_email_user(pool: &PgPool, display_name: &str, roles: &[&str]) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO users (id, email, password_hash, display_name, avatar_url)
        VALUES ($1, $2, 'hashed-pw', $3, '')
        "#,
    )
    .bind(id)
    .bind(format!("{id}@example.test"))
    .bind(display_name)
    .execute(pool)
    .await
    .expect("seed email user 应成功");

    seed_user_roles(pool, id, roles).await;
    id
}

async fn seed_user_roles(pool: &PgPool, user_id: Uuid, roles: &[&str]) {
    for role in roles {
        sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, $2)")
            .bind(user_id)
            .bind(role)
            .execute(pool)
            .await
            .expect("seed user role 应成功");
    }
}

fn admin_token(state: &AppState, id: Uuid) -> String {
    state
        .admin_token_manager
        .generate(id, AdminRole::Admin.as_str())
        .expect("签 admin token 应成功")
}

async fn get(state: &AppState, uri: &str, token: &str) -> (StatusCode, String) {
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[sqlx::test]
async fn user_list_uses_admin_users_path_and_documents_contract(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool).await;
    let token = admin_token(&state, admin_id);
    let alice_id = seed_user(&pool, "Alice 学员", &["teacher", "student"]).await;
    seed_user(&pool, "Bob 教师", &["teacher"]).await;

    let (status, body) = get(
        &state,
        "/api/v1/admin/users?role=student&q=Alice&registered_from=2020-01-01T00:00:00Z&registered_to=2030-01-01T00:00:00Z&page=1&page_size=20",
        &token,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "用户列表应返回 200：{body}");
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["items"].as_array().unwrap().len(), 1);
    assert_eq!(json["items"][0]["id"], alice_id.to_string());
    assert_eq!(json["items"][0]["display_name"], "Alice 学员");
    assert_eq!(
        json["items"][0]["roles"],
        serde_json::json!(["student", "teacher"])
    );
    assert_eq!(json["items"][0]["status"], "active");
    assert_eq!(json["items"][0]["avatar_url"], "");
    assert_eq!(json["page"]["page"], 1);
    assert_eq!(json["page"]["page_size"], 20);
    assert_eq!(json["page"]["total"], 1);

    let (old_status, _) = get(&state, "/api/v1/admin/admins/users", &token).await;
    assert_eq!(
        old_status,
        StatusCode::NOT_FOUND,
        "用户列表不应误挂在 /api/v1/admin/admins/users"
    );
}

#[sqlx::test]
async fn user_list_rejects_inverted_registration_interval(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool).await;
    let token = admin_token(&state, admin_id);

    let (status, body) = get(
        &state,
        "/api/v1/admin/users?registered_from=2030-01-01T00:00:00Z&registered_to=2020-01-01T00:00:00Z",
        &token,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "反向时间区间应返回 400：{body}"
    );
}

#[sqlx::test]
async fn user_list_omits_absent_phone_and_email_instead_of_returning_null(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool).await;
    let token = admin_token(&state, admin_id);
    let phone_user_id = seed_user(&pool, "手机用户", &["student"]).await;
    let email_user_id = seed_email_user(&pool, "邮箱用户", &["teacher"]).await;

    let (status, body) = get(&state, "/api/v1/admin/users?page=1&page_size=20", &token).await;

    assert_eq!(status, StatusCode::OK, "用户列表应返回 200：{body}");
    let json: Value = serde_json::from_str(&body).unwrap();
    let items = json["items"].as_array().expect("items 应为数组");

    let phone_user = items
        .iter()
        .find(|item| item["id"] == phone_user_id.to_string())
        .expect("列表应包含手机用户")
        .as_object()
        .expect("手机用户应为对象");
    assert!(phone_user["phone"].is_string());
    assert!(
        !phone_user.contains_key("email"),
        "纯手机用户不应返回 email:null"
    );

    let email_user = items
        .iter()
        .find(|item| item["id"] == email_user_id.to_string())
        .expect("列表应包含邮箱用户")
        .as_object()
        .expect("邮箱用户应为对象");
    assert!(email_user["email"].is_string());
    assert!(
        !email_user.contains_key("phone"),
        "纯邮箱用户不应返回 phone:null"
    );
}
