mod account_deletion_support;
mod avatar_support;
use avatar_support::{picture, setup, upload};
use tsz_rust::{
    avatar::{dto::AvatarUploadRequest, service},
    platform::storage::{ObjectKey, PutOptions},
};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::state::AppState;
use uuid::Uuid;

async fn user(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id,email,phone,password_hash,display_name) VALUES ($1,$2,$3,'hash','Avatar')",
    )
    .bind(id)
    .bind(format!("{id}@example.test")).bind(format!("139{:08}", id.as_u128() % 100_000_000))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO user_roles (user_id,role) VALUES ($1,'student')")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn request(
    state: AppState,
    token: Option<String>,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = tsz_rust::router(state)
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
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
async fn avatar_writes_require_web_identity(pool: PgPool) {
    let state = AppState::for_test(pool);
    for uri in ["/api/v1/me/avatar/upload-url", "/api/v1/me/avatar"] {
        let (status, _) = request(
            state.clone(),
            None,
            uri,
            json!({"key":"unknown","content_type":"image/png","size":10}),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let token = state
            .admin_token_manager
            .generate(Uuid::now_v7(), "admin")
            .unwrap();
        let (status, _) = request(
            state.clone(),
            Some(token),
            uri,
            json!({"key":"unknown","content_type":"image/png","size":10}),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[sqlx::test]
async fn avatar_without_storage_is_explicitly_disabled(pool: PgPool) {
    let id = user(&pool).await;
    let state = AppState::for_test(pool);
    let token = state.token_manager.generate(id, "student").unwrap();
    let (status, body) = request(
        state,
        Some(token),
        "/api/v1/me/avatar/upload-url",
        json!({"content_type":"image/png","size":10}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert_eq!(body["code"], "avatar_storage_not_configured");
}

#[sqlx::test]
async fn account_deletion_handler_schedules_avatar_cleanup_before_delete(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    service::confirm(&state, &auth, &source).await.unwrap();
    let id: Uuid = sqlx::query_scalar("SELECT avatar_upload_id FROM users WHERE id=$1")
        .bind(auth.subject)
        .fetch_one(&pool)
        .await
        .unwrap();
    let token = state
        .token_manager
        .generate(auth.subject, "student")
        .unwrap();
    let (status, body) = request(
        state.clone(),
        Some(token.clone()),
        "/api/v1/auth/account/deletion-code",
        json!({"channel":"email"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let pending = tsz_rust::account_deletion::service::create(
        &state,
        &auth,
        serde_json::from_value(account_deletion_support::input(Uuid::now_v7(), "0")).unwrap(),
    )
    .await
    .unwrap();
    account_deletion_support::deadline(&pool, pending.id, -1).await;
    assert!(
        tsz_rust::account_deletion::service::complete(&pool, auth.subject, pending.id)
            .await
            .unwrap()
    );
    assert_eq!(
        read(&state, &format!("/api/v1/avatars/{id}"), None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let tasks:i64=sqlx::query_scalar("SELECT count(*) FROM avatar_cleanup_tasks WHERE upload_id=$1 AND kind='canonical' AND status='pending'").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(tasks, 1);
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT user_id FROM avatar_uploads WHERE id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(owner, None);
}

async fn read(state: &AppState, uri: &str, token: Option<&str>) -> axum::response::Response {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[sqlx::test]
async fn confirmed_images_are_public_and_profile_wire_stays_compatible(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    sqlx::query("UPDATE users SET email=NULL,phone='13800138000' WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_roles (user_id,role) VALUES ($1,'teacher')")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    let token = state
        .token_manager
        .generate(auth.subject, "student")
        .unwrap();
    for (format, content_type) in [
        (image::ImageFormat::Jpeg, "image/jpeg"),
        (image::ImageFormat::Png, "image/png"),
        (image::ImageFormat::WebP, "image/webp"),
    ] {
        let bytes = picture(format);
        let (status, body) = request(
            state.clone(),
            Some(token.clone()),
            "/api/v1/me/avatar/upload-url",
            json!({"content_type":content_type,"size":bytes.len()}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["upload"]["max_bytes"], 5242880);
        let remaining = body["upload"]["expires_in"].as_u64().unwrap();
        assert!(remaining > 0 && remaining <= 600);
        assert_eq!(body["upload"]["headers"]["content-type"], content_type);
        assert!(
            body["upload"]["url"]
                .as_str()
                .unwrap()
                .starts_with("memory://")
        );
        let key = body["upload"]["key"].as_str().unwrap();
        let id: Uuid = sqlx::query_scalar("SELECT id FROM avatar_uploads WHERE source_key=$1")
            .bind(key)
            .fetch_one(&pool)
            .await
            .unwrap();
        let public = format!("/api/v1/avatars/{id}");
        let pending = read(&state, &public, None).await;
        assert_eq!(pending.status(), StatusCode::NOT_FOUND);
        assert_eq!(pending.headers()[header::CACHE_CONTROL], "no-store");
        store
            .put(
                &ObjectKey::parse(key).unwrap(),
                bytes,
                PutOptions::default(),
            )
            .await
            .unwrap();
        let (status, confirmed) = request(
            state.clone(),
            Some(token.clone()),
            "/api/v1/me/avatar",
            json!({"key":key}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{confirmed}");
        assert_eq!(confirmed["user"]["id"], auth.subject.to_string());
        assert_eq!(confirmed["user"]["phone"], "13800138000");
        assert!(confirmed["user"].get("email").is_none());
        assert_eq!(confirmed["user"]["roles"].as_array().unwrap().len(), 2);
        assert_eq!(confirmed["user"]["active_role"], "student");
        assert!(
            confirmed["user"]["avatar_url"]
                .as_str()
                .unwrap()
                .ends_with(&id.to_string())
        );
        let me = read(&state, "/api/v1/auth/me", Some(&token)).await;
        let me: Value =
            serde_json::from_slice(&me.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(me, confirmed["user"]);
        let public_image = read(&state, &public, None).await;
        assert_eq!(public_image.status(), StatusCode::OK);
        assert_eq!(public_image.headers()[header::CONTENT_TYPE], "image/webp");
        assert_eq!(
            public_image.headers()[header::CACHE_CONTROL],
            "public, max-age=300"
        );
        assert_eq!(
            public_image.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        let image = public_image.into_body().collect().await.unwrap().to_bytes();
        let image = image::load_from_memory(&image).unwrap();
        assert_eq!((image.width(), image.height()), (512, 512));
        assert_eq!(
            read(&state, &format!("/api/v1/{key}"), None).await.status(),
            StatusCode::NOT_FOUND
        );
    }
}

#[sqlx::test]
async fn invalid_inputs_ownership_expiry_and_quota_are_rejected(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let token = state
        .token_manager
        .generate(auth.subject, "student")
        .unwrap();
    for (body, status, code) in [
        (
            json!({"content_type":"image/gif","size":1}),
            StatusCode::BAD_REQUEST,
            "unsupported_avatar_content_type",
        ),
        (
            json!({"content_type":"image/png","size":0}),
            StatusCode::BAD_REQUEST,
            "invalid_avatar_size",
        ),
        (
            json!({"content_type":"image/png","size":5242881}),
            StatusCode::PAYLOAD_TOO_LARGE,
            "avatar_file_too_large",
        ),
        (
            json!({"content_type":"image/png","size":1.5}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request_body",
        ),
    ] {
        let (actual, error) = request(
            state.clone(),
            Some(token.clone()),
            "/api/v1/me/avatar/upload-url",
            body,
        )
        .await;
        assert_eq!(actual, status, "{error}");
        assert_eq!(error["code"], code);
    }
    let key = upload(&state, &auth, store.as_ref()).await;
    let other = user(&pool).await;
    let other_token = state.token_manager.generate(other, "student").unwrap();
    for key in [&key, "uploads/avatars/unknown/original.png"] {
        let (status, error) = request(
            state.clone(),
            Some(other_token.clone()),
            "/api/v1/me/avatar",
            json!({"key":key}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["code"], "invalid_avatar_key");
    }
    let id: Uuid = sqlx::query_scalar("SELECT id FROM avatar_uploads WHERE source_key=$1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE avatar_uploads SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let (status, error) = request(
        state.clone(),
        Some(token.clone()),
        "/api/v1/me/avatar",
        json!({"key":key}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["code"], "invalid_avatar_key");
    for _ in 0..2 {
        service::create_upload(
            &state,
            &auth,
            AvatarUploadRequest {
                content_type: "image/png".into(),
                size: 1,
            },
        )
        .await
        .unwrap();
    }
    let (status, error) = request(
        state.clone(),
        Some(token.clone()),
        "/api/v1/me/avatar/upload-url",
        json!({"content_type":"image/png","size":1}),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error["code"], "avatar_upload_rate_limited");
    assert_eq!(
        service::current_profile(&state, &auth)
            .await
            .unwrap()
            .avatar_url,
        ""
    );
}

#[sqlx::test]
async fn actual_bytes_are_validated_and_failed_confirm_keeps_old_avatar(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let token = state
        .token_manager
        .generate(auth.subject, "student")
        .unwrap();
    let previous = upload(&state, &auth, store.as_ref()).await;
    let old = service::confirm(&state, &auth, &previous)
        .await
        .unwrap()
        .avatar_url;
    let permit = service::create_upload(
        &state,
        &auth,
        AvatarUploadRequest {
            content_type: "image/png".into(),
            size: 8,
        },
    )
    .await
    .unwrap()
    .upload;
    let (status, error) = request(
        state.clone(),
        Some(token.clone()),
        "/api/v1/me/avatar",
        json!({"key":permit.key}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "avatar_upload_not_completed");
    for (bytes, code) in [
        (b"x".to_vec(), "invalid_avatar_size"),
        (b"notimage".to_vec(), "unsupported_avatar_content_type"),
        (b"\x89PNG\r\n\x1a\n".to_vec(), "avatar_invalid_image"),
    ] {
        store
            .put(
                &ObjectKey::parse(&permit.key).unwrap(),
                bytes,
                PutOptions::default(),
            )
            .await
            .unwrap();
        let (status, error) = request(
            state.clone(),
            Some(token.clone()),
            "/api/v1/me/avatar",
            json!({"key":permit.key}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        assert_eq!(error["code"], code);
        assert_eq!(
            service::current_profile(&state, &auth)
                .await
                .unwrap()
                .avatar_url,
            old
        );
    }
    let (status, error) = request(
        state.clone(),
        Some(token.clone()),
        "/api/v1/me/avatar",
        json!({"key":previous}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(error["user"]["avatar_url"], old);
}
