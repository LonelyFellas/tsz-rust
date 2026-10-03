mod avatar_support;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::{
    state::AppState,
    user::{
        model::UserRole,
        repository::{NewUser, UserRepository},
    },
};
use uuid::Uuid;

async fn create_user(pool: &PgPool, role: UserRole) -> Uuid {
    UserRepository::new(pool.clone())
        .create(NewUser {
            id: Uuid::now_v7(),
            phone: Some("13800138000".into()),
            email: None,
            password_hash: "private-hash".into(),
            display_name: "Before".into(),
            first_role: role,
            registration_ip: None,
        })
        .await
        .unwrap()
        .id
}

async fn patch(state: &AppState, token: Option<&str>, body: String) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("PATCH")
        .uri("/api/v1/me")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(request.body(Body::from(body)).unwrap())
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
async fn saves_own_name_and_returns_safe_complete_profile(pool: PgPool) {
    let id = create_user(&pool, UserRole::Teacher).await;
    let state = AppState::for_test(pool.clone());
    let token = state.token_manager.generate(id, "teacher").unwrap();
    let (status, body) = patch(
        &state,
        Some(&token),
        json!({"display_name": "  新昵称😀  "}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"user": {
            "id": id, "display_name": "新昵称😀", "phone": "13800138000",
            "avatar_url": "", "roles": ["teacher"], "active_role": "teacher"
        }})
    );
    let saved = UserRepository::new(pool).get_by_id(&id).await.unwrap();
    assert_eq!(saved.display_name, "新昵称😀");
    assert_eq!(saved.password_hash, "private-hash");
    assert_eq!(saved.security_version, 0);
}

#[sqlx::test]
async fn valid_boundaries_retry_and_auth_me_are_compatible(pool: PgPool) {
    let id = create_user(&pool, UserRole::Student).await;
    let state = AppState::for_test(pool.clone());
    let token = state.token_manager.generate(id, "student").unwrap();
    for name in [
        "字".repeat(50),
        "😀".repeat(50),
        "O'Brien & \"友\"".into(),
        "\u{0085}昵称\u{0085}".into(),
    ] {
        for _ in 0..2 {
            let (status, body) = patch(
                &state,
                Some(&token),
                json!({"display_name": name}).to_string(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["user"]["display_name"], name.trim());
        }
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/me")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["display_name"], "昵称");
    assert!(body.get("user").is_none());
    assert!(body.get("email").is_none());
    assert_eq!(
        UserRepository::new(pool)
            .get_by_id(&id)
            .await
            .unwrap()
            .security_version,
        0
    );
}

#[sqlx::test]
async fn rejects_invalid_names_and_strict_json_without_writes(pool: PgPool) {
    let id = create_user(&pool, UserRole::Student).await;
    let state = AppState::for_test(pool.clone());
    let token = state.token_manager.generate(id, "student").unwrap();
    for name in [
        "".into(),
        " \t\n".into(),
        "字".repeat(51),
        "😀".repeat(51),
        "<b>".into(),
        "a\u{0000}b".into(),
        "a\u{0085}b".into(),
        "\u{feff}昵称".into(),
        "👨\u{200d}👩".into(),
    ] {
        let (status, body) = patch(
            &state,
            Some(&token),
            json!({"display_name": name}).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name:?}: {body}");
        assert_eq!(body["code"], "invalid_display_name");
        assert_eq!(body["field"], "display_name");
    }
    for body in [
        json!({}),
        json!({"display_name": null}),
        json!({"display_name": 5}),
        json!({"display_name": "ok", "id": id}),
        json!({"display_name": "ok", "phone": "13900139000"}),
        json!({"display_name": "ok", "avatar_url": "x"}),
        json!({"display_name": "ok", "roles": ["teacher"]}),
        json!({"display_name": "ok", "email": "x@y.com"}),
        json!({"display_name": "ok", "active_role": "teacher"}),
    ] {
        let (status, body) = patch(&state, Some(&token), body.to_string()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "invalid_request_body");
    }
    let (status, body) = patch(&state, Some(&token), "{".into()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_json");
    let (status, body) = patch(
        &state,
        Some(&token),
        json!({"display_name": "a".repeat(2100)}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["code"], "payload_too_large");
    assert_eq!(
        UserRepository::new(pool)
            .get_by_id(&id)
            .await
            .unwrap()
            .display_name,
        "Before"
    );
}

#[sqlx::test]
async fn rejects_anonymous_admin_expired_and_stale_sessions(pool: PgPool) {
    use chrono::Duration;
    use tsz_rust::auth::{Realm, TokenManager};
    let id = create_user(&pool, UserRole::Student).await;
    let state = AppState::for_test(pool.clone());
    let admin = TokenManager::new("test-secret", Realm::Admin, Duration::minutes(15))
        .generate(id, "student")
        .unwrap();
    let expired = TokenManager::new("test-secret", Realm::Web, Duration::seconds(-3600))
        .generate(id, "student")
        .unwrap();
    let stale = state
        .token_manager
        .generate_with_version(id, "student", 1)
        .unwrap();
    let ghost = state
        .token_manager
        .generate(Uuid::now_v7(), "student")
        .unwrap();
    for token in [
        None,
        Some(admin.as_str()),
        Some(expired.as_str()),
        Some(stale.as_str()),
        Some(ghost.as_str()),
    ] {
        let (status, body) = patch(&state, token, json!({"display_name": "No"}).to_string()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "invalid_token");
    }
    sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let token = state.token_manager.generate(id, "student").unwrap();
    let (status, body) = patch(
        &state,
        Some(&token),
        json!({"display_name": "No"}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "invalid_token");
    assert_eq!(
        UserRepository::new(pool)
            .get_by_id(&id)
            .await
            .unwrap()
            .display_name,
        "Before"
    );
}

#[sqlx::test]
async fn rechecks_security_after_http_authentication(pool: PgPool) {
    for change in [
        "UPDATE users SET status = 'disabled' WHERE id = $1",
        "UPDATE users SET security_version = security_version + 1 WHERE id = $1",
        "DELETE FROM users WHERE id = $1",
    ] {
        let id = create_user(&pool, UserRole::Student).await;
        let state = AppState::for_test(pool.clone());
        let token = state.token_manager.generate(id, "student").unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let body = Body::from_stream(futures_util::stream::once(async move {
            entered_tx.send(()).unwrap();
            release_rx.await.unwrap();
            Ok::<_, std::io::Error>(json!({"display_name": "Not allowed"}).to_string())
        }));
        let request = Request::builder()
            .method("PATCH")
            .uri("/api/v1/me")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(body)
            .unwrap();
        let task = tokio::spawn(tsz_rust::router(state).oneshot(request));
        tokio::time::timeout(std::time::Duration::from_secs(10), entered_rx)
            .await
            .unwrap()
            .unwrap();
        sqlx::query(change).bind(id).execute(&pool).await.unwrap();
        release_tx.send(()).unwrap();
        let response = task.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["code"], "invalid_token");
        let name = sqlx::query_scalar::<_, String>("SELECT display_name FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(name.is_none() || name.as_deref() == Some("Before"));
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[sqlx::test]
async fn profile_assembly_failure_after_commit_allows_safe_retry(pool: PgPool) {
    let id = create_user(&pool, UserRole::Student).await;
    let state = AppState::for_test(pool.clone());
    let token = state.token_manager.generate(id, "student").unwrap();
    sqlx::raw_sql("CREATE FUNCTION hide_roles_after_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN ALTER TABLE user_roles RENAME TO hidden_user_roles; RETURN NEW; END $$; CREATE TRIGGER hide_roles AFTER UPDATE OF display_name ON users FOR EACH STATEMENT EXECUTE FUNCTION hide_roles_after_update();").execute(&pool).await.unwrap();
    let request = json!({"display_name": "Committed"}).to_string();
    let (status, body) = patch(&state, Some(&token), request.clone()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], "internal_error");
    assert_eq!(body["detail"], "internal error");
    assert!(!body.to_string().contains("user_roles"));
    let saved = UserRepository::new(pool.clone())
        .get_by_id(&id)
        .await
        .unwrap();
    assert_eq!(saved.display_name, "Committed");
    assert_eq!(saved.security_version, 0);
    sqlx::raw_sql(
        "DROP TRIGGER hide_roles ON users; ALTER TABLE hidden_user_roles RENAME TO user_roles;",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (status, body) = patch(&state, Some(&token), request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["display_name"], "Committed");
    assert_eq!(body["user"]["roles"], json!(["student"]));
}

#[sqlx::test]
async fn nickname_and_avatar_updates_preserve_each_other_and_retry_reads_current_profile(
    pool: PgPool,
) {
    let (state, auth, store) = avatar_support::setup(&pool).await;
    let token = state
        .token_manager
        .generate(auth.subject, "student")
        .unwrap();
    let first_key = avatar_support::upload(&state, &auth, store.as_ref()).await;
    let (status, _) = patch(
        &state,
        Some(&token),
        json!({"display_name": "Before avatar"}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let first = tsz_rust::avatar::service::confirm(&state, &auth, &first_key)
        .await
        .unwrap();
    assert_eq!(first.display_name, "Before avatar");
    assert!(!first.avatar_url.is_empty());
    let (status, body) = patch(
        &state,
        Some(&token),
        json!({"display_name": "After avatar"}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["avatar_url"], first.avatar_url);

    let second_key = avatar_support::upload(&state, &auth, store.as_ref()).await;
    let ((status, _), confirmed) = tokio::join!(
        patch(
            &state,
            Some(&token),
            json!({"display_name": "Concurrent😀"}).to_string(),
        ),
        tsz_rust::avatar::service::confirm(&state, &auth, &second_key),
    );
    assert_eq!(status, StatusCode::OK);
    let confirmed = confirmed.unwrap();
    assert_ne!(confirmed.avatar_url, first.avatar_url);
    let saved = UserRepository::new(pool.clone())
        .get_by_id(&auth.subject)
        .await
        .unwrap();
    assert_eq!(saved.display_name, "Concurrent😀");
    assert_eq!(saved.avatar_url, confirmed.avatar_url);
    assert_eq!(saved.security_version, 0);
    let retried = tsz_rust::avatar::service::confirm(&state, &auth, &first_key)
        .await
        .unwrap();
    assert_eq!(retried.display_name, saved.display_name);
    assert_eq!(retried.avatar_url, saved.avatar_url);
    let response = tsz_rust::router(state)
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/me")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["display_name"], saved.display_name);
    assert_eq!(body["avatar_url"], saved.avatar_url);
    assert!(body.get("password_hash").is_none());
}

#[test]
fn openapi_declares_strict_request_and_wrapped_profile() {
    use utoipa::OpenApi;
    let spec = serde_json::to_value(tsz_rust::openapi::ApiDoc::openapi()).unwrap();
    for (path, method, statuses) in [
        (
            "/api/v1/me/avatar/upload-url",
            "post",
            vec!["500", "501", "503"],
        ),
        (
            "/api/v1/avatars/{id}",
            "get",
            vec!["404", "500", "501", "503"],
        ),
    ] {
        for status in statuses {
            assert!(spec["paths"][path][method]["responses"][status].is_object());
        }
    }
    let operation = &spec["paths"]["/api/v1/me"]["patch"];
    assert_eq!(
        operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/UpdateProfileResponse"
    );
    assert_eq!(
        spec["components"]["schemas"]["UpdateProfileRequest"]["additionalProperties"],
        false
    );
    assert_eq!(
        spec["components"]["schemas"]["UpdateProfileRequest"]["required"],
        json!(["display_name"])
    );
    assert_eq!(
        spec["paths"]["/api/v1/auth/me"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/UserProfile"
    );
}
