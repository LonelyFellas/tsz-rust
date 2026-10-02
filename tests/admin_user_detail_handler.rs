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
    if role == AdminRole::Admin {
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'users.access',$1),($1,'users.read_sensitive',$1)")
            .bind(id).execute(pool).await.unwrap();
    }
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
async fn delegated_user_actions_are_independent_and_redact_write_responses(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::Admin).await;
    let target = seed_user(&pool, "原昵称", &["student"]).await;
    let bearer = token(&state, admin, AdminRole::Admin);
    let uri = format!("/api/v1/admin/users/{target}");
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='users.read_sensitive'")
        .bind(admin).execute(&pool).await.unwrap();
    let (status, body) = request(&state, "GET", &uri, Some(&bearer), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    assert!(response.get("phone").is_none());
    assert!(response.get("email").is_none());
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'users.edit',$1)")
        .bind(admin).execute(&pool).await.unwrap();
    let (status, body) = request(
        &state,
        "PATCH",
        &uri,
        Some(&bearer),
        Some(json!({"display_name":"新昵称"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        serde_json::from_str::<Value>(&body)
            .unwrap()
            .get("phone")
            .is_none()
    );
    let (status, _) = request(
        &state,
        "PATCH",
        &format!("{uri}/status"),
        Some(&bearer),
        Some(json!({"status":"disabled"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        stored_user(&pool, target).await,
        ("新昵称".into(), "active".into())
    );
    sqlx::query(
        "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='users.edit'",
    )
    .bind(admin)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'users.set_status',$1)")
        .bind(admin).execute(&pool).await.unwrap();
    let (status, body) = request(
        &state,
        "PATCH",
        &format!("{uri}/status"),
        Some(&bearer),
        Some(json!({"status":"disabled"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        serde_json::from_str::<Value>(&body)
            .unwrap()
            .get("phone")
            .is_none()
    );
    let (status, _) = request(
        &state,
        "PATCH",
        &uri,
        Some(&bearer),
        Some(json!({"display_name":"不应修改"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        stored_user(&pool, target).await,
        ("新昵称".into(), "disabled".into())
    );
    // 业务授权不等同于管理员治理授权。
    let (status, _) = request(&state, "GET", "/api/v1/admin/admins", Some(&bearer), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn user_service_rechecks_revoked_permission_and_account_status(pool: PgPool) {
    use tsz_rust::admin::accounts::{AdminAccountsRepository, AdminAccountsService};
    use tsz_rust::user::{model::UserStatus, repository::UserRepository};
    let admin = seed_admin(&pool, AdminRole::Admin).await;
    let target = seed_user(&pool, "未改动", &["student"]).await;
    let service = AdminAccountsService::new(
        AdminAccountsRepository::new(pool.clone()),
        Some(UserRepository::new(pool.clone())),
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'users.edit',$1),($1,'users.set_status',$1)")
        .bind(admin).execute(&pool).await.unwrap();
    service
        .set_user_display_name(admin, &target, "首次修改")
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM admins WHERE id=$1 FOR UPDATE")
        .bind(admin)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='users.edit'",
    )
    .bind(admin)
    .execute(&mut *tx)
    .await
    .unwrap();
    let operation = tokio::spawn(async move {
        service
            .set_user_display_name(admin, &target, "撤权后不应修改")
            .await
    });
    tx.commit().await.unwrap();
    let error = operation.await.unwrap().unwrap_err();
    assert_eq!(
        std::error::Error::source(&error)
            .unwrap()
            .downcast_ref::<tsz_rust::error::AppError>()
            .unwrap()
            .code(),
        tsz_rust::error::ErrorCode::Forbidden
    );
    assert_eq!(stored_user(&pool, target).await.0, "首次修改");
    sqlx::query("UPDATE admins SET status='disabled' WHERE id=$1")
        .bind(admin)
        .execute(&pool)
        .await
        .unwrap();
    let service = AdminAccountsService::new(
        AdminAccountsRepository::new(pool.clone()),
        Some(UserRepository::new(pool.clone())),
    );
    let error = service
        .set_user_status(admin, &target, UserStatus::Disabled)
        .await
        .unwrap_err();
    assert_eq!(
        std::error::Error::source(&error)
            .unwrap()
            .downcast_ref::<tsz_rust::error::AppError>()
            .unwrap()
            .code(),
        tsz_rust::error::ErrorCode::AccountDisabled
    );
    assert_eq!(stored_user(&pool, target).await.1, "active");
}

#[sqlx::test]
async fn user_write_holds_authorization_lock_until_business_commit(pool: PgPool) {
    use tsz_rust::admin::accounts::{AdminAccountsRepository, AdminAccountsService};
    use tsz_rust::user::repository::UserRepository;
    let actor = seed_admin(&pool, AdminRole::Admin).await;
    let target = seed_user(&pool, "原昵称", &["student"]).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'users.edit',$1)")
        .bind(actor).execute(&pool).await.unwrap();
    let service = AdminAccountsService::new(
        AdminAccountsRepository::new(pool.clone()),
        Some(UserRepository::new(pool.clone())),
    );
    let mut object_lock = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(target)
        .execute(&mut *object_lock)
        .await
        .unwrap();
    let write = tokio::spawn(async move {
        service
            .set_user_display_name(actor, &target, "写事务获胜")
            .await
    });
    // 等待真实业务写阻塞，而非用固定 sleep 猜测调度。pg_stat_activity 仅用于测试同步。
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%WITH updated AS%')")
                .fetch_one(&pool).await.unwrap();
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let revoke_pool = pool.clone();
    let mut revoke = tokio::spawn(async move {
        let mut tx = revoke_pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM admins WHERE id=$1 FOR UPDATE")
            .bind(actor)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='users.edit'",
        )
        .bind(actor)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut revoke)
            .await
            .is_err(),
        "撤权不能在敏感写提交前穿过管理员共享锁"
    );
    object_lock.commit().await.unwrap();
    write.await.unwrap().unwrap();
    revoke.await.unwrap();
    assert_eq!(stored_user(&pool, target).await.0, "写事务获胜");
    let service = AdminAccountsService::new(
        AdminAccountsRepository::new(pool.clone()),
        Some(UserRepository::new(pool.clone())),
    );
    let error = service
        .set_user_display_name(actor, &target, "不应修改")
        .await
        .unwrap_err();
    assert_eq!(
        std::error::Error::source(&error)
            .unwrap()
            .downcast_ref::<tsz_rust::error::AppError>()
            .unwrap()
            .code(),
        tsz_rust::error::ErrorCode::Forbidden
    );
    assert_eq!(stored_user(&pool, target).await.0, "写事务获胜");
}

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
async fn teacher_verification_is_preserved_by_detail_and_updates(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::SuperAdmin).await;
    let user = seed_user(&pool, "已认证教师", &["student", "teacher"]).await;
    let bearer = token(&state, admin, AdminRole::SuperAdmin);
    sqlx::query("INSERT INTO teacher_profiles (user_id, verified) VALUES ($1, true)")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();

    for (method, path, body) in [
        ("GET", format!("/api/v1/admin/users/{user}"), None),
        (
            "PATCH",
            format!("/api/v1/admin/users/{user}"),
            Some(json!({"display_name": "新昵称"})),
        ),
        (
            "PATCH",
            format!("/api/v1/admin/users/{user}/status"),
            Some(json!({"status": "disabled"})),
        ),
        (
            "PATCH",
            format!("/api/v1/admin/users/{user}/status"),
            Some(json!({"status": "active"})),
        ),
    ] {
        let (status, body) = request(&state, method, &path, Some(&bearer), body).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let response: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(response["teacher_verified"], true, "{body}");
        assert_eq!(response["roles"], json!(["student", "teacher"]));
        assert_eq!(response["phone"], user.as_u128().to_string());
    }
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
async fn disabled_user_cannot_login_refresh_or_access_protected_routes_and_can_recover(
    pool: PgPool,
) {
    use tsz_rust::{
        session::{repository::RefreshTokenRepository, service::SessionService},
        user::{
            repository::UserRepository,
            service::{RegisterInput, UserService},
        },
    };

    let state = AppState::for_test(pool.clone());
    let admin = seed_admin(&pool, AdminRole::SuperAdmin).await;
    let bearer = token(&state, admin, AdminRole::SuperAdmin);
    let user = UserService::new(UserRepository::new(pool.clone()))
        .register(RegisterInput {
            phone: Some("13800138000".to_owned()),
            email: None,
            password: "Violet!River7294Cloud".to_owned(),
        })
        .await
        .unwrap();
    let login_body = json!({"identifier": "13800138000", "password": "Violet!River7294Cloud"});
    let (status, body) = request(
        &state,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(login_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let login: Value = serde_json::from_str(&body).unwrap();
    let access = login["access_token"].as_str().unwrap();
    let refresh = SessionService::new(RefreshTokenRepository::new(pool.clone()), state.refresh_ttl)
        .issue(user.id)
        .await
        .unwrap();
    let (status, body) = request(&state, "GET", "/api/v1/auth/me", Some(access), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let before: Value = serde_json::from_str(&body).unwrap();
    let uri = format!("/api/v1/admin/users/{}/status", user.id);

    let (status, body) = request(
        &state,
        "PATCH",
        &uri,
        Some(&bearer),
        Some(json!({"status": "disabled"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = request(&state, "GET", "/api/v1/auth/me", Some(access), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = request(
        &state,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(login_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["code"],
        "account_disabled"
    );
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/refresh")
                .header(
                    header::COOKIE,
                    format!("refresh_token={}", refresh.plaintext),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let (status, body) = request(
        &state,
        "PATCH",
        &uri,
        Some(&bearer),
        Some(json!({"status": "active"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) =
        request(&state, "POST", "/api/v1/auth/login", None, Some(login_body)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let login: Value = serde_json::from_str(&body).unwrap();
    let (status, body) = request(
        &state,
        "GET",
        "/api/v1/auth/me",
        login["access_token"].as_str(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let after: Value = serde_json::from_str(&body).unwrap();
    for field in ["id", "phone", "email", "roles", "display_name"] {
        assert_eq!(after[field], before[field], "{field}");
    }
    assert_eq!(after["phone"], "13800138000");
    assert_eq!(after["roles"], json!(["student"]));
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
