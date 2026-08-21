//! admin 三期「管理 web 用户」的三条写/读端点契约（设计 §11 表 + `AdminUser` 形状）：
//!   `GET /users/{id}` / `PATCH /users/{id}/status`(super) / `PATCH /users/{id}`(super)。
//!
//! 形状契约的硬指标（设计 §11 补充点）：`phone`/`email` 缺值时**必须省略键**，
//! 不得返回 null 或 ""；三条端点与列表逐字段同形状，前端可拿响应直接替换列表里那一行。

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
    admin::{AdminRepository, AdminRole, NewAdmin},
    state::AppState,
};

async fn seed_admin(pool: &PgPool, role: AdminRole) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.as_u128().to_string(),
            display_name: "测试管理员".to_owned(),
            password_hash: "hashed-pw".to_owned(),
            role,
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
    .bind(id.as_u128().to_string())
    .bind(display_name)
    .execute(pool)
    .await
    .expect("seed user 应成功");

    for role in roles {
        sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, $2)")
            .bind(id)
            .bind(role)
            .execute(pool)
            .await
            .expect("seed user role 应成功");
    }
    id
}

async fn seed_email_user(pool: &PgPool, display_name: &str) -> Uuid {
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
    id
}

fn token(state: &AppState, id: Uuid, role: AdminRole) -> String {
    state
        .admin_token_manager
        .generate(id, role.as_str())
        .expect("签 admin token 应成功")
}

async fn request(
    state: &AppState,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };

    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn stored_user(pool: &PgPool, id: Uuid) -> (String, String) {
    sqlx::query_as::<_, (String, String)>("SELECT display_name, status FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("目标用户应存在")
}

// ————————————————————— GET /users/{id} —————————————————————

#[sqlx::test]
async fn plain_admin_can_read_user_detail(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::Admin).await;
    let user = seed_user(&pool, "李雷", &["teacher", "student"]).await;
    let bearer = token(&state, admin, AdminRole::Admin);

    let (status, body) = request(
        &state,
        "GET",
        &format!("/api/v1/admin/users/{user}"),
        Some(&bearer),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["id"], user.to_string());
    assert_eq!(response["display_name"], "李雷");
    assert_eq!(response["status"], "active");
    assert_eq!(response["avatar_url"], "");
    // 角色顺序与列表一致：student 在前。
    assert_eq!(response["roles"], json!(["student", "teacher"]));
    // 防泄：C 端密码哈希绝不出现在 admin 视图上。
    assert!(response.get("password_hash").is_none());
}

#[sqlx::test]
async fn detail_omits_missing_contact_keys(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::Admin).await;
    let phone_only = seed_user(&pool, "只有手机号", &["student"]).await;
    let email_only = seed_email_user(&pool, "只有邮箱").await;
    let bearer = token(&state, admin, AdminRole::Admin);

    let (_, phone_body) = request(
        &state,
        "GET",
        &format!("/api/v1/admin/users/{phone_only}"),
        Some(&bearer),
        None,
    )
    .await;
    let (_, email_body) = request(
        &state,
        "GET",
        &format!("/api/v1/admin/users/{email_only}"),
        Some(&bearer),
        None,
    )
    .await;

    let phone_user: Value = serde_json::from_str(&phone_body).unwrap();
    let email_user: Value = serde_json::from_str(&email_body).unwrap();
    // 缺值必须**省略键**，不得是 null 或 ""——前端类型是 `phone?: string`。
    assert!(phone_user.get("email").is_none(), "{phone_body}");
    assert!(phone_user["phone"].is_string());
    assert!(email_user.get("phone").is_none(), "{email_body}");
    assert!(email_user["email"].is_string());
}

#[sqlx::test]
async fn detail_of_unknown_user_is_not_found(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::Admin).await;
    let bearer = token(&state, admin, AdminRole::Admin);

    let (status, body) = request(
        &state,
        "GET",
        &format!("/api/v1/admin/users/{}", Uuid::now_v7()),
        Some(&bearer),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let problem: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(problem["code"], "not_found");
}

#[sqlx::test]
async fn detail_rejects_malformed_id(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::Admin).await;
    let bearer = token(&state, admin, AdminRole::Admin);

    let (status, _) = request(
        &state,
        "GET",
        "/api/v1/admin/users/not-a-uuid",
        Some(&bearer),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn detail_requires_authentication(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let user = seed_user(&pool, "李雷", &["student"]).await;

    let (status, _) = request(
        &state,
        "GET",
        &format!("/api/v1/admin/users/{user}"),
        None,
        None,
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ————————————————————— PATCH /users/{id}/status —————————————————————

#[sqlx::test]
async fn super_admin_disables_user_and_response_reflects_new_value(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::SuperAdmin).await;
    let user = seed_user(&pool, "李雷", &["student"]).await;
    let bearer = token(&state, admin, AdminRole::SuperAdmin);

    let (status, body) = request(
        &state,
        "PATCH",
        &format!("/api/v1/admin/users/{user}/status"),
        Some(&bearer),
        Some(json!({ "status": "disabled" })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    // 回读必须给出**改动后**的值：数据修改型 CTE 的写入对同一语句其余部分不可见，
    // 回读写成 JOIN users 会吐出旧快照——这条断言就是钉死那个陷阱的。
    assert_eq!(response["status"], "disabled");
    assert_eq!(response["display_name"], "李雷");
    assert_eq!(response["roles"], json!(["student"]));
    assert_eq!(stored_user(&pool, user).await.1, "disabled");
}

#[sqlx::test]
async fn super_admin_reenables_user(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::SuperAdmin).await;
    let user = seed_user(&pool, "李雷", &["student"]).await;
    let bearer = token(&state, admin, AdminRole::SuperAdmin);
    let uri = format!("/api/v1/admin/users/{user}/status");

    request(
        &state,
        "PATCH",
        &uri,
        Some(&bearer),
        Some(json!({ "status": "disabled" })),
    )
    .await;
    let (status, body) = request(
        &state,
        "PATCH",
        &uri,
        Some(&bearer),
        Some(json!({ "status": "active" })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stored_user(&pool, user).await.1, "active");
}

#[sqlx::test]
async fn plain_admin_cannot_change_user_status(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::Admin).await;
    let user = seed_user(&pool, "李雷", &["student"]).await;
    let bearer = token(&state, admin, AdminRole::Admin);

    let (status, _) = request(
        &state,
        "PATCH",
        &format!("/api/v1/admin/users/{user}/status"),
        Some(&bearer),
        Some(json!({ "status": "disabled" })),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(stored_user(&pool, user).await.1, "active");
}

#[sqlx::test]
async fn user_status_outside_enum_is_rejected(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::SuperAdmin).await;
    let user = seed_user(&pool, "李雷", &["student"]).await;
    let bearer = token(&state, admin, AdminRole::SuperAdmin);

    let (status, body) = request(
        &state,
        "PATCH",
        &format!("/api/v1/admin/users/{user}/status"),
        Some(&bearer),
        Some(json!({ "status": "deleted" })),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(stored_user(&pool, user).await.1, "active");
}

#[sqlx::test]
async fn status_update_on_unknown_user_is_not_found(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::SuperAdmin).await;
    let bearer = token(&state, admin, AdminRole::SuperAdmin);

    let (status, body) = request(
        &state,
        "PATCH",
        &format!("/api/v1/admin/users/{}/status", Uuid::now_v7()),
        Some(&bearer),
        Some(json!({ "status": "disabled" })),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

// ————————————————————— PATCH /users/{id} —————————————————————

#[sqlx::test]
async fn super_admin_renames_user(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::SuperAdmin).await;
    let user = seed_user(&pool, "旧昵称", &["student"]).await;
    let bearer = token(&state, admin, AdminRole::SuperAdmin);

    let (status, body) = request(
        &state,
        "PATCH",
        &format!("/api/v1/admin/users/{user}"),
        Some(&bearer),
        Some(json!({ "display_name": "  新昵称  " })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    // trim 与 C 端注册同一套 DisplayName::parse。
    assert_eq!(response["display_name"], "新昵称");
    assert_eq!(response["status"], "active");
    assert_eq!(stored_user(&pool, user).await.0, "新昵称");
}

#[sqlx::test]
async fn rename_only_touches_display_name(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::SuperAdmin).await;
    let user = seed_user(&pool, "旧昵称", &["student"]).await;
    let bearer = token(&state, admin, AdminRole::SuperAdmin);
    request(
        &state,
        "PATCH",
        &format!("/api/v1/admin/users/{user}/status"),
        Some(&bearer),
        Some(json!({ "status": "disabled" })),
    )
    .await;

    let (status, body) = request(
        &state,
        "PATCH",
        &format!("/api/v1/admin/users/{user}"),
        Some(&bearer),
        Some(json!({ "display_name": "新昵称" })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let (display_name, stored_status) = stored_user(&pool, user).await;
    assert_eq!(display_name, "新昵称");
    assert_eq!(stored_status, "disabled", "改昵称不得顺手把状态刷回 active");
}

#[sqlx::test]
async fn invalid_display_name_is_rejected(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::SuperAdmin).await;
    let user = seed_user(&pool, "旧昵称", &["student"]).await;
    let bearer = token(&state, admin, AdminRole::SuperAdmin);

    for invalid in ["", "   ", "<script>", "李\u{200b}雷"] {
        let (status, body) = request(
            &state,
            "PATCH",
            &format!("/api/v1/admin/users/{user}"),
            Some(&bearer),
            Some(json!({ "display_name": invalid })),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid:?} → {body}");
        let problem: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(problem["code"], "invalid_display_name");
        assert_eq!(problem["field"], "display_name");
    }
    assert_eq!(stored_user(&pool, user).await.0, "旧昵称");
}

#[sqlx::test]
async fn plain_admin_cannot_rename_user(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::Admin).await;
    let user = seed_user(&pool, "旧昵称", &["student"]).await;
    let bearer = token(&state, admin, AdminRole::Admin);

    let (status, _) = request(
        &state,
        "PATCH",
        &format!("/api/v1/admin/users/{user}"),
        Some(&bearer),
        Some(json!({ "display_name": "新昵称" })),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(stored_user(&pool, user).await.0, "旧昵称");
}

#[sqlx::test]
async fn rename_of_unknown_user_is_not_found(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::SuperAdmin).await;
    let bearer = token(&state, admin, AdminRole::SuperAdmin);

    let (status, body) = request(
        &state,
        "PATCH",
        &format!("/api/v1/admin/users/{}", Uuid::now_v7()),
        Some(&bearer),
        Some(json!({ "display_name": "新昵称" })),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[sqlx::test]
async fn rename_requires_authentication(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let user = seed_user(&pool, "旧昵称", &["student"]).await;

    let (status, _) = request(
        &state,
        "PATCH",
        &format!("/api/v1/admin/users/{user}"),
        None,
        Some(json!({ "display_name": "新昵称" })),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(stored_user(&pool, user).await.0, "旧昵称");
}
