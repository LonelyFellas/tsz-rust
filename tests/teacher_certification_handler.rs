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

async fn user(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, email, phone, password_hash, display_name) VALUES ($1, $2, $3, 'hash', '学生')")
        .bind(id).bind(format!("{id}@example.test")).bind(format!("139{:08}", id.as_u128() % 100_000_000)).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, 'student')")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn admin(pool: &PgPool, role: AdminRole) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.as_u128().to_string(),
            display_name: "审核员".into(),
            password_hash: "hash".into(),
            role,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    id
}

async fn get(state: AppState, uri: &str, token: Option<String>) -> (StatusCode, Value) {
    let mut request = Request::builder().uri(uri);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = tsz_rust::router(state)
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn grant(pool: &PgPool, id: Uuid, keys: &[&str]) {
    for key in keys {
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,$2,$1)")
            .bind(id).bind(key).execute(pool).await.unwrap();
    }
}

#[sqlx::test]
async fn delegated_teacher_access_review_revoke_do_not_imply_sensitive_materials(pool: PgPool) {
    let owner = user(&pool).await;
    let reviewer = admin(&pool, AdminRole::Admin).await;
    grant(
        &pool,
        reviewer,
        &["teacherapply.access", "teacherapply.review"],
    )
    .await;
    let state = AppState::for_test(pool.clone());
    let owner_token = state.token_manager.generate(owner, "student").unwrap();
    let bearer = state
        .admin_token_manager
        .generate(reviewer, "admin")
        .unwrap();
    let payload = application_body(&pool, owner).await;
    let file = payload["id_front"].as_str().unwrap().to_owned();
    let (_, submitted) = send(
        &state,
        "POST",
        "/api/v1/me/teacher-certification/applications",
        &owner_token,
        payload,
    )
    .await;
    let application = submitted["id"].as_str().unwrap();
    let detail_path = format!("/api/v1/admin/teacher-applications/{application}");
    for path in [
        "/api/v1/admin/teacher-applications".to_owned(),
        detail_path.clone(),
    ] {
        let (status, body) = get(state.clone(), &path, Some(bearer.clone())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let app = if path == detail_path {
            &body["application"]
        } else {
            &body["items"][0]
        };
        for field in ["real_name", "contact", "statement"] {
            assert_eq!(app[field], "", "{body}");
        }
        assert_eq!(app["review_reason"], Value::Null);
        if path == detail_path {
            assert_eq!(body["files"], json!([]));
        }
    }
    let file_path = format!("/api/v1/admin/teacher-certification/files/{file}");
    assert_eq!(
        get(state.clone(), &file_path, Some(bearer.clone())).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, reviewed) = send(
        &state,
        "POST",
        &format!("{detail_path}/review"),
        &bearer,
        json!({"decision":"approve"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reviewed}");
    assert_eq!(reviewed["status"], "approved");
    assert_eq!(reviewed["contact"], "");
    let revoke_path = format!("/api/v1/admin/users/{owner}/teacher-certification");
    assert_eq!(
        send(
            &state,
            "DELETE",
            &revoke_path,
            &bearer,
            json!({"reason":"复核"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    grant(&pool, reviewer, &["teacherapply.read_sensitive"]).await;
    let (status, detail) = get(state.clone(), &detail_path, Some(bearer.clone())).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["application"]["contact"], "teacher@example.test");
    assert_eq!(detail["files"].as_array().unwrap().len(), 4);
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key IN ('teacherapply.review','teacherapply.read_sensitive')")
        .bind(reviewer).execute(&pool).await.unwrap();
    grant(&pool, reviewer, &["teacherapply.revoke"]).await;
    assert_eq!(
        send(
            &state,
            "DELETE",
            &revoke_path,
            &bearer,
            json!({"reason":"复核"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, error) = send(
        &state,
        "POST",
        &format!("{detail_path}/review"),
        &bearer,
        json!({"decision":"reject","reason":"撤权后不应审核"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
    assert_eq!(error["code"], "forbidden");
    // 有 revoke 但状态已撤销时仍保留原业务冲突，不误报权限。
    let (status, error) = send(
        &state,
        "DELETE",
        &revoke_path,
        &bearer,
        json!({"reason":"复核"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["code"], "revision_conflict");
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='teacherapply.revoke'")
        .bind(reviewer).execute(&pool).await.unwrap();
    let (status, error) = send(
        &state,
        "DELETE",
        &revoke_path,
        &bearer,
        json!({"reason":"撤权后复核"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
    assert_eq!(error["code"], "forbidden");
    let (_, own) = get(
        state.clone(),
        "/api/v1/me/teacher-certification",
        Some(owner_token),
    )
    .await;
    assert_eq!(own["application"]["contact"], "teacher@example.test");
    assert_eq!(own["files"].as_array().unwrap().len(), 4);
    assert_eq!(
        get(state, "/api/v1/me/teacher-certification", Some(bearer))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn certification_requires_login(pool: PgPool) {
    let (status, _) = get(
        AppState::for_test(pool),
        "/api/v1/me/teacher-certification",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn student_can_read_their_unverified_state(pool: PgPool) {
    let id = user(&pool).await;
    let state = AppState::for_test(pool);
    let token = state.token_manager.generate(id, "student").unwrap();
    let (status, body) = get(state, "/api/v1/me/teacher-certification", Some(token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["teacher_verified"], false);
    assert_eq!(body["application"], Value::Null);
}

#[sqlx::test]
async fn ordinary_admin_cannot_read_applications(pool: PgPool) {
    let id = admin(&pool, AdminRole::Admin).await;
    let state = AppState::for_test(pool);
    let token = state.admin_token_manager.generate(id, "admin").unwrap();
    let (status, _) = get(state, "/api/v1/admin/teacher-applications", Some(token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn application_body(pool: &PgPool, owner: Uuid) -> Value {
    let mut ids = Vec::new();
    for kind in ["id_front", "id_back", "education", "language"] {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO teacher_certification_files (id, user_id, kind, object_key, content_type, size_bytes, state) VALUES ($1, $2, $3, $4, 'image/png', 68, 'ready')")
            .bind(id).bind(owner).bind(kind).bind(id.to_string()).execute(pool).await.unwrap();
        ids.push(id);
    }
    json!({ "real_name": "李老师", "contact": "teacher@example.test", "statement": "申请任教",
        "id_front": ids[0], "id_back": ids[1], "education_files": [ids[2]], "language_files": [ids[3]] })
}

#[sqlx::test]
async fn application_review_revoke_preserves_student_and_notifies_once(pool: PgPool) {
    let id = user(&pool).await;
    let reviewer = admin(&pool, AdminRole::SuperAdmin).await;
    let state = AppState::for_test(pool.clone());
    let token = state.token_manager.generate(id, "student").unwrap();
    let admin_token = state
        .admin_token_manager
        .generate(reviewer, "super_admin")
        .unwrap();
    let payload = application_body(&pool, id).await;
    let (status, submitted) = send(
        &state,
        "POST",
        "/api/v1/me/teacher-certification/applications",
        &token,
        payload.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{submitted}");
    assert_eq!(submitted["status"], "pending");
    let application = submitted["id"].as_str().unwrap();
    let (duplicate, _) = send(
        &state,
        "POST",
        "/api/v1/me/teacher-certification/applications",
        &token,
        payload,
    )
    .await;
    assert_eq!(duplicate, StatusCode::CONFLICT);
    let review_url = format!("/api/v1/admin/teacher-applications/{application}/review");
    let (status, approved) = send(
        &state,
        "POST",
        &review_url,
        &admin_token,
        json!({"decision":"approve"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    assert_eq!(approved["status"], "approved");
    let (status, _) = send(
        &state,
        "POST",
        &review_url,
        &admin_token,
        json!({"decision":"approve"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, cert) = get(
        state.clone(),
        "/api/v1/me/teacher-certification",
        Some(token.clone()),
    )
    .await;
    assert_eq!(cert["teacher_verified"], true);
    let roles: Vec<String> =
        sqlx::query_scalar("SELECT role FROM user_roles WHERE user_id = $1 ORDER BY role")
            .bind(id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(roles, ["student", "teacher"]);
    let (_, notifications) = get(
        state.clone(),
        "/api/v1/me/notifications",
        Some(token.clone()),
    )
    .await;
    assert_eq!(notifications["items"].as_array().unwrap().len(), 1);
    assert_eq!(notifications["items"][0]["kind"], "teacher_approved");
    let revoke_url = format!("/api/v1/admin/users/{id}/teacher-certification");
    let (status, _) = send(
        &state,
        "DELETE",
        &revoke_url,
        &admin_token,
        json!({"reason":"  "}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = send(
        &state,
        "DELETE",
        &revoke_url,
        &admin_token,
        json!({"reason":"资格复核未通过"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, cert) = get(
        state.clone(),
        "/api/v1/me/teacher-certification",
        Some(token.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cert["teacher_verified"], false);
    assert_eq!(cert["application"]["status"], "revoked");
    let roles: Vec<String> =
        sqlx::query_scalar("SELECT role FROM user_roles WHERE user_id = $1 ORDER BY role")
            .bind(id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(roles, ["student"]);
    let (_, notifications) = get(
        state.clone(),
        "/api/v1/me/notifications",
        Some(token.clone()),
    )
    .await;
    assert_eq!(notifications["items"].as_array().unwrap().len(), 2);
    assert_eq!(notifications["items"][0]["reason"], "资格复核未通过");
    let (status, _) = send(
        &state,
        "POST",
        "/api/v1/me/teacher-certification/applications",
        &token,
        application_body(&pool, id).await,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[sqlx::test]
async fn other_users_files_cannot_be_attached(pool: PgPool) {
    let owner = user(&pool).await;
    let attacker = user(&pool).await;
    let state = AppState::for_test(pool.clone());
    let token = state.token_manager.generate(attacker, "student").unwrap();
    let (status, _) = send(
        &state,
        "POST",
        "/api/v1/me/teacher-certification/applications",
        &token,
        application_body(&pool, owner).await,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM teacher_applications")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn rejection_requires_reason_and_keeps_history_on_resubmit(pool: PgPool) {
    let id = user(&pool).await;
    let reviewer = admin(&pool, AdminRole::SuperAdmin).await;
    let state = AppState::for_test(pool.clone());
    let token = state.token_manager.generate(id, "student").unwrap();
    let admin_token = state
        .admin_token_manager
        .generate(reviewer, "super_admin")
        .unwrap();
    let (_, application) = send(
        &state,
        "POST",
        "/api/v1/me/teacher-certification/applications",
        &token,
        application_body(&pool, id).await,
    )
    .await;
    let id1 = application["id"]
        .as_str()
        .expect("application must be created");
    let path = format!("/api/v1/admin/teacher-applications/{id1}/review");
    let (status, _) = send(
        &state,
        "POST",
        &path,
        &admin_token,
        json!({"decision":"reject"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, rejected) = send(
        &state,
        "POST",
        &path,
        &admin_token,
        json!({"decision":"reject", "reason":"材料不清晰"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rejected["review_reason"], "材料不清晰");
    let (status, next) = send(
        &state,
        "POST",
        "/api/v1/me/teacher-certification/applications",
        &token,
        application_body(&pool, id).await,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(next["id"], application["id"]);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM teacher_applications WHERE user_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 2);
}

fn with_storage(mut state: AppState) -> AppState {
    use std::time::Duration;
    use tsz_rust::platform::storage::{
        MemoryAdapter, StoragePolicy, StoragePrivacy, StorageRegistry, StorageSpace,
    };
    let policy = StoragePolicy::new(
        StoragePrivacy::Private,
        10 * 1024 * 1024,
        Duration::from_secs(60),
        None,
    )
    .unwrap();
    state.object_storage = StorageRegistry::from_stores([MemoryAdapter::object_store(
        StorageSpace::parse("teacher-certification").unwrap(),
        policy,
    )])
    .unwrap();
    state
}

use tsz_rust::platform::storage::{
    ObjectKey, ObjectMetadata, ObjectStore, PresignedRequest, PutOptions, StorageError,
    StoragePolicy, StorageRegistry, StorageSpace,
};

struct PausedMaterialStore {
    inner: std::sync::Arc<dyn ObjectStore>,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl ObjectStore for PausedMaterialStore {
    fn space(&self) -> &StorageSpace {
        self.inner.space()
    }
    fn policy(&self) -> &StoragePolicy {
        self.inner.policy()
    }
    async fn put(
        &self,
        key: &ObjectKey,
        body: Vec<u8>,
        options: PutOptions,
    ) -> Result<ObjectMetadata, StorageError> {
        self.inner.put(key, body, options).await
    }
    async fn read(&self, key: &ObjectKey) -> Result<Vec<u8>, StorageError> {
        let body = self.inner.read(key).await?;
        self.started.notify_one();
        self.release.notified().await;
        Ok(body)
    }
    async fn stat(&self, key: &ObjectKey) -> Result<ObjectMetadata, StorageError> {
        self.inner.stat(key).await
    }
    async fn presign_read(&self, key: &ObjectKey) -> Result<PresignedRequest, StorageError> {
        self.inner.presign_read(key).await
    }
    async fn presign_write(
        &self,
        key: &ObjectKey,
        length: u64,
        options: PutOptions,
    ) -> Result<PresignedRequest, StorageError> {
        self.inner.presign_write(key, length, options).await
    }
    async fn copy(
        &self,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<ObjectMetadata, StorageError> {
        self.inner.copy(source, destination).await
    }
    async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }
}

#[sqlx::test]
async fn admin_material_read_requires_submission_and_rechecks_revocation_after_storage_await(
    pool: PgPool,
) {
    let owner = user(&pool).await;
    let reviewer = admin(&pool, AdminRole::Admin).await;
    grant(
        &pool,
        reviewer,
        &["teacherapply.access", "teacherapply.read_sensitive"],
    )
    .await;
    let mut state = with_storage(AppState::for_test(pool.clone()));
    let owner_token = state.token_manager.generate(owner, "student").unwrap();
    let bearer = state
        .admin_token_manager
        .generate(reviewer, "admin")
        .unwrap();
    let (_, file) = upload(&state, &owner_token, "image/png", PNG).await;
    let file_id: Uuid = file["id"].as_str().unwrap().parse().unwrap();
    let path = format!("/api/v1/admin/teacher-certification/files/{file_id}");
    assert_eq!(
        get(state.clone(), &path, Some(bearer.clone())).await.0,
        StatusCode::NOT_FOUND
    );
    let application = Uuid::now_v7();
    sqlx::query("INSERT INTO teacher_applications(id,user_id,real_name,contact,statement) VALUES ($1,$2,'教师','teacher@example.test','任教')")
        .bind(application).bind(owner).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO teacher_application_files(application_id,file_id,position) VALUES ($1,$2,0)",
    )
    .bind(application)
    .bind(file_id)
    .execute(&pool)
    .await
    .unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri(&path)
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        PNG
    );
    let paused = std::sync::Arc::new(PausedMaterialStore {
        inner: state
            .object_storage
            .get(&StorageSpace::parse("teacher-certification").unwrap())
            .unwrap(),
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    state.object_storage =
        StorageRegistry::from_stores([paused.clone() as std::sync::Arc<dyn ObjectStore>]).unwrap();
    for (mutation, expected) in [
        (
            "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='teacherapply.read_sensitive'",
            "forbidden",
        ),
        (
            "UPDATE admins SET status='disabled' WHERE id=$1",
            "account_disabled",
        ),
        (
            "UPDATE admins SET must_change_password=true WHERE id=$1",
            "must_change_password",
        ),
    ] {
        sqlx::query("UPDATE admins SET status='active',must_change_password=false WHERE id=$1")
            .bind(reviewer)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'teacherapply.read_sensitive',$1) ON CONFLICT DO NOTHING")
            .bind(reviewer).execute(&pool).await.unwrap();
        let request_state = state.clone();
        let request_path = path.clone();
        let request_token = bearer.clone();
        let request =
            tokio::spawn(
                async move { get(request_state, &request_path, Some(request_token)).await },
            );
        tokio::time::timeout(std::time::Duration::from_secs(5), paused.started.notified())
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM admins WHERE id=$1 FOR UPDATE")
            .bind(reviewer)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(mutation)
            .bind(reviewer)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        paused.release.notify_one();
        let (status, body) = request.await.unwrap();
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["code"], expected);
    }
}

const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 255, 255, 255, 127, 0,
    9, 251, 3, 253, 42, 134, 227, 138, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

async fn upload(
    state: &AppState,
    token: &str,
    content_type: &str,
    bytes: &[u8],
) -> (StatusCode, Value) {
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/me/teacher-certification/files?kind=id_front")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(bytes.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[sqlx::test]
async fn uploaded_material_is_private_and_ownership_checked(pool: PgPool) {
    let owner = user(&pool).await;
    let other = user(&pool).await;
    let state = with_storage(AppState::for_test(pool.clone()));
    let token = state.token_manager.generate(owner, "student").unwrap();
    let other_token = state.token_manager.generate(other, "student").unwrap();
    let (status, file) = upload(&state, &token, "image/png", PNG).await;
    assert_eq!(status, StatusCode::CREATED, "{file}");
    assert!(file.get("object_key").is_none());
    assert!(file.get("url").is_none());
    let path = format!(
        "/api/v1/me/teacher-certification/files/{}",
        file["id"].as_str().unwrap()
    );
    let (status, _) = get(state.clone(), &path, Some(other_token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri(&path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        PNG
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM teacher_certification_files WHERE user_id = $1 AND state = 'ready'",
    )
    .bind(owner)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let (status, _) = send(&state, "DELETE", &path, &token, json!({})).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = get(state, &path, Some(token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn forged_image_payload_is_rejected_without_ready_file(pool: PgPool) {
    let id = user(&pool).await;
    let state = with_storage(AppState::for_test(pool.clone()));
    let token = state.token_manager.generate(id, "student").unwrap();
    let (status, _) = upload(&state, &token, "image/png", b"<script>alert(1)</script>").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM teacher_certification_files WHERE state = 'ready'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn application_details_and_notifications_are_owner_scoped(pool: PgPool) {
    let id = user(&pool).await;
    let other = user(&pool).await;
    let reviewer = admin(&pool, AdminRole::SuperAdmin).await;
    let state = AppState::for_test(pool.clone());
    let token = state.token_manager.generate(id, "student").unwrap();
    let other_token = state.token_manager.generate(other, "student").unwrap();
    let admin_token = state
        .admin_token_manager
        .generate(reviewer, "super_admin")
        .unwrap();
    let (_, app) = send(
        &state,
        "POST",
        "/api/v1/me/teacher-certification/applications",
        &token,
        application_body(&pool, id).await,
    )
    .await;
    let app_id = app["id"].as_str().unwrap();
    let path = format!("/api/v1/me/teacher-certification/applications/{app_id}");
    let (status, detail) = get(state.clone(), &path, Some(token.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["files"].as_array().unwrap().len(), 4);
    let (status, _) = get(state.clone(), &path, Some(other_token.clone())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &state,
        "POST",
        &format!("/api/v1/admin/teacher-applications/{app_id}/review"),
        &admin_token,
        json!({"decision":"reject","reason":"请补充材料"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = get(
        state.clone(),
        "/api/v1/me/notifications?page=1&page_size=1",
        Some(token.clone()),
    )
    .await;
    assert_eq!(list["unread_count"], 1);
    let notification = list["items"][0]["id"].as_str().unwrap();
    let path = format!("/api/v1/me/notifications/{notification}/read");
    let (status, _) = send(&state, "PATCH", &path, &other_token, json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, read) = send(&state, "PATCH", &path, &token, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(read["read_at"].is_string());
    let (_, list) = get(state, "/api/v1/me/notifications", Some(token)).await;
    assert_eq!(list["unread_count"], 0);
}

#[sqlx::test]
async fn concurrent_reviews_have_one_winner_and_one_notification(pool: PgPool) {
    let id = user(&pool).await;
    let reviewer = admin(&pool, AdminRole::SuperAdmin).await;
    let state = AppState::for_test(pool.clone());
    let token = state.token_manager.generate(id, "student").unwrap();
    let admin_token = state
        .admin_token_manager
        .generate(reviewer, "super_admin")
        .unwrap();
    let (_, app) = send(
        &state,
        "POST",
        "/api/v1/me/teacher-certification/applications",
        &token,
        application_body(&pool, id).await,
    )
    .await;
    let path = format!(
        "/api/v1/admin/teacher-applications/{}/review",
        app["id"].as_str().unwrap()
    );
    let (a, b) = tokio::join!(
        send(
            &state,
            "POST",
            &path,
            &admin_token,
            json!({"decision":"approve"})
        ),
        send(
            &state,
            "POST",
            &path,
            &admin_token,
            json!({"decision":"reject","reason":"需重传"})
        )
    );
    let mut statuses = [a.0.as_u16(), b.0.as_u16()];
    statuses.sort();
    assert_eq!(statuses, [200, 409]);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM user_notifications WHERE user_id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn expired_and_deleted_account_material_is_reclaimed(pool: PgPool) {
    use tsz_rust::platform::storage::{ObjectKey, StorageSpace};
    let id = user(&pool).await;
    let state = with_storage(AppState::for_test(pool.clone()));
    let token = state.token_manager.generate(id, "student").unwrap();
    let (status, file) = upload(&state, &token, "image/png", PNG).await;
    assert_eq!(status, StatusCode::CREATED);
    let file_id: Uuid = file["id"].as_str().unwrap().parse().unwrap();
    let key: String =
        sqlx::query_scalar("SELECT object_key FROM teacher_certification_files WHERE id = $1")
            .bind(file_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("UPDATE teacher_certification_files SET expires_at = now() - interval '1 second' WHERE id = $1").bind(file_id).execute(&pool).await.unwrap();
    assert_eq!(
        tsz_rust::teacher_certification::cleanup::sweep(&state)
            .await
            .unwrap(),
        1
    );
    let storage = state
        .object_storage
        .get(&StorageSpace::parse("teacher-certification").unwrap())
        .unwrap();
    assert!(storage.stat(&ObjectKey::parse(key).unwrap()).await.is_err());
    let (_, next) = upload(&state, &token, "image/png", PNG).await;
    let next_id: Uuid = next["id"].as_str().unwrap().parse().unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        tsz_rust::teacher_certification::cleanup::sweep(&state)
            .await
            .unwrap(),
        1
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM teacher_certification_files WHERE id = $1")
            .bind(next_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn deleting_an_account_does_not_race_an_inflight_material_upload(pool: PgPool) {
    let owner = user(&pool).await;
    let state = with_storage(AppState::for_test(pool.clone()));
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO teacher_certification_files (id,user_id,kind,object_key,content_type,size_bytes) VALUES ($1,$2,'id_front',$3,'image/png',68)")
        .bind(id).bind(owner).bind(format!("materials/{id}")).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        tsz_rust::teacher_certification::cleanup::sweep(&state)
            .await
            .unwrap(),
        0
    );
    let status: String =
        sqlx::query_scalar("SELECT state FROM teacher_certification_files WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "uploading");
    sqlx::query(
        "UPDATE teacher_certification_files SET expires_at=now()-interval '1 second' WHERE id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        tsz_rust::teacher_certification::cleanup::sweep(&state)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test]
async fn ordinary_admin_cannot_read_materials_review_or_revoke(pool: PgPool) {
    let owner = user(&pool).await;
    let reviewer = admin(&pool, AdminRole::Admin).await;
    let state = with_storage(AppState::for_test(pool.clone()));
    let owner_token = state.token_manager.generate(owner, "student").unwrap();
    let admin_token = state
        .admin_token_manager
        .generate(reviewer, "admin")
        .unwrap();
    let body = application_body(&pool, owner).await;
    let file_id = body["id_front"].as_str().unwrap();
    let (_, application) = send(
        &state,
        "POST",
        "/api/v1/me/teacher-certification/applications",
        &owner_token,
        body.clone(),
    )
    .await;
    let id = application["id"].as_str().unwrap();
    for path in [
        format!("/api/v1/admin/teacher-applications/{id}"),
        format!("/api/v1/admin/teacher-certification/files/{file_id}"),
    ] {
        assert_eq!(
            get(state.clone(), &path, Some(admin_token.clone())).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        send(
            &state,
            "POST",
            &format!("/api/v1/admin/teacher-applications/{id}/review"),
            &admin_token,
            json!({"decision":"approve"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &state,
            "DELETE",
            &format!("/api/v1/admin/users/{owner}/teacher-certification"),
            &admin_token,
            json!({"reason":"不应有权限"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, mine) = get(state, "/api/v1/me/teacher-certification", Some(owner_token)).await;
    assert_eq!(mine["application"]["status"], "pending");
    assert_eq!(mine["teacher_verified"], false);
}

#[sqlx::test]
async fn migration_rollback_refuses_to_orphan_stored_materials(pool: PgPool) {
    let owner = user(&pool).await;
    application_body(&pool, owner).await;
    let error = sqlx::raw_sql(include_str!(
        "../migrations/20260928010000_teacher_certification.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("teacher certification data must be retained")
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM teacher_certification_files")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 4);
}

#[sqlx::test]
async fn oversized_material_returns_problem_details(pool: PgPool) {
    let owner = user(&pool).await;
    let state = with_storage(AppState::for_test(pool));
    let token = state.token_manager.generate(owner, "student").unwrap();
    let bytes = vec![0u8; 10 * 1024 * 1024 + 1];
    let (status, body) = upload(&state, &token, "image/png", &bytes).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["code"], "payload_too_large");
}

#[test]
fn certification_openapi_query_parameters_match_runtime() {
    use utoipa::OpenApi;
    let spec = serde_json::to_value(tsz_rust::openapi::ApiDoc::openapi()).unwrap();
    for path in [
        "/api/v1/admin/teacher-applications",
        "/api/v1/me/notifications",
    ] {
        let params = spec["paths"][path]["get"]["parameters"].as_array().unwrap();
        for name in ["page", "page_size"] {
            let param = params.iter().find(|p| p["name"] == name).unwrap();
            assert_eq!(param["in"], "query");
            assert_eq!(param["required"], false);
            assert_eq!(param["schema"]["minimum"], 1);
            if name == "page_size" {
                assert_eq!(param["schema"]["maximum"], 100);
            }
        }
        assert!(params.iter().all(|p| p["in"] == "query"));
    }
    let kind = &spec["paths"]["/api/v1/me/teacher-certification/files"]["post"]["parameters"][0];
    assert_eq!(kind["name"], "kind");
    assert_eq!(kind["in"], "query");
    assert_eq!(kind["required"], true);
}

#[test]
fn certification_openapi_only_declares_supported_image_media() {
    use utoipa::OpenApi;
    let spec = serde_json::to_value(tsz_rust::openapi::ApiDoc::openapi()).unwrap();
    let upload =
        &spec["paths"]["/api/v1/me/teacher-certification/files"]["post"]["requestBody"]["content"];
    let own = &spec["paths"]["/api/v1/me/teacher-certification/files/{id}"]["get"]["responses"]["200"]
        ["content"];
    let admin = &spec["paths"]["/api/v1/admin/teacher-certification/files/{id}"]["get"]["responses"]
        ["200"]["content"];
    for content in [upload, own, admin] {
        assert_eq!(content.as_object().unwrap().len(), 3);
        for mime in ["image/jpeg", "image/png", "image/webp"] {
            assert!(content.get(mime).is_some(), "missing {mime}");
            assert_eq!(content[mime]["schema"]["type"], "string");
            assert_eq!(content[mime]["schema"]["format"], "binary");
        }
    }
}

#[sqlx::test]
async fn revoking_legacy_teacher_only_account_keeps_student_membership(pool: PgPool) {
    let owner = user(&pool).await;
    let reviewer = admin(&pool, AdminRole::SuperAdmin).await;
    sqlx::query("UPDATE user_roles SET role='teacher' WHERE user_id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET last_active_role='teacher' WHERE id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO teacher_profiles (user_id,verified) VALUES ($1,true)")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let state = AppState::for_test(pool.clone());
    let token = state
        .admin_token_manager
        .generate(reviewer, "super_admin")
        .unwrap();
    let path = format!("/api/v1/admin/users/{owner}/teacher-certification");
    assert_eq!(
        send(
            &state,
            "DELETE",
            &path,
            &token,
            json!({"reason":"历史资格复核"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let roles: Vec<String> =
        sqlx::query_scalar("SELECT role::text FROM user_roles WHERE user_id=$1 ORDER BY role")
            .bind(owner)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(roles, vec!["student"]);
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1)")
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(exists);
    assert_eq!(
        send(
            &state,
            "DELETE",
            &path,
            &token,
            json!({"reason":"重复撤销"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM user_notifications WHERE user_id=$1")
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
