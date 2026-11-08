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

async fn user(pool: &PgPool, role: UserRole) -> Uuid {
    let id = Uuid::now_v7();
    UserRepository::new(pool.clone())
        .create(NewUser {
            id,
            phone: None,
            email: Some(format!("{id}@example.test")),
            password_hash: "unused".into(),
            display_name: "i18n fixture".into(),
            first_role: role,
            registration_ip: None,
        })
        .await
        .unwrap()
        .id
}

async fn request(
    state: &AppState,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, bool) {
    let mut req = Request::builder()
        .method(if body.is_some() { "PATCH" } else { "GET" })
        .uri(path);
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let resp = tsz_rust::router(state.clone())
        .oneshot(
            req.header(header::CONTENT_TYPE, "application/json")
                .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let no_store = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .is_some_and(|v| v == "no-store");
    (
        status,
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap(),
        no_store,
    )
}

#[sqlx::test]
async fn capabilities_are_public_surface_scoped_and_reject_ambiguous_query(pool: PgPool) {
    let state = AppState::for_test(pool);
    for surface in ["web", "admin"] {
        let (s, b, no_store) = request(
            &state,
            &format!("/api/v1/i18n/capabilities?surface={surface}"),
            None,
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert!(no_store);
        assert_eq!(
            b,
            json!({"schema_version":1,"preference_version":1,"default_locale":"zh-Hans","supported_locales":["zh-Hans"]})
        );
    }
    for query in [
        "",
        "?surface=",
        "?surface=future",
        "?surface=web&surface=admin",
    ] {
        let (s, b, _) = request(
            &state,
            &format!("/api/v1/i18n/capabilities{query}"),
            None,
            None,
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(b["code"], "invalid_query");
    }
}

#[sqlx::test]
async fn email_only_users_can_save_clear_and_reread_without_onboarding(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    for role in [UserRole::Student, UserRole::Teacher] {
        let id = user(&pool, role).await;
        let token = state.token_manager.generate(id, role.as_str()).unwrap();
        let (s, b, n) = request(&state, "/api/v1/me/preferences", Some(&token), None).await;
        assert_eq!(s, StatusCode::OK);
        assert!(n);
        assert_eq!(b["preferences"]["interface_locale"], Value::Null);
        for locale in [json!("zh-Hans"), Value::Null] {
            let (s, b, n) = request(
                &state,
                "/api/v1/me/preferences",
                Some(&token),
                Some(json!({"interface_locale":locale})),
            )
            .await;
            assert_eq!(s, StatusCode::OK, "{b}");
            assert!(n);
            let (_, read, _) = request(&state, "/api/v1/me/preferences", Some(&token), None).await;
            assert_eq!(read["preferences"]["interface_locale"], locale);
        }
    }
}

#[sqlx::test]
async fn invalid_or_disabled_languages_do_not_write_or_affect_other_accounts(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let a = user(&pool, UserRole::Student).await;
    let b = user(&pool, UserRole::Student).await;
    let token = state.token_manager.generate(a, "student").unwrap();
    for body in [
        json!({}),
        json!({"interface_locale":"zh-CN"}),
        json!({"interface_locale":{"zh-Hans":null}}),
        json!({"interface_locale":"zh-Hans","user_id":b}),
        json!({"interface_locale":1}),
    ] {
        let (s, p, _) = request(&state, "/api/v1/me/preferences", Some(&token), Some(body)).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(p["code"], "invalid_request_body");
    }
    let (s, p, _) = request(
        &state,
        "/api/v1/me/preferences",
        Some(&token),
        Some(json!({"interface_locale":"en-US"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(p["code"], "unsupported_language");
    request(
        &state,
        "/api/v1/me/preferences",
        Some(&token),
        Some(json!({"interface_locale":"zh-Hans"})),
    )
    .await;
    let other = state.token_manager.generate(b, "student").unwrap();
    let (_, read, _) = request(&state, "/api/v1/me/preferences", Some(&other), None).await;
    assert_eq!(read["preferences"]["interface_locale"], Value::Null);
    sqlx::query("UPDATE users SET interface_locale='en-US' WHERE id=$1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    let (_, read, _) = request(&state, "/api/v1/me/preferences", Some(&token), None).await;
    assert_eq!(
        read["preferences"]["interface_locale"], "en-US",
        "停用语言仍保留存储值"
    );
}

#[sqlx::test]
async fn preferences_require_current_web_identity(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = user(&pool, UserRole::Teacher).await;
    let valid = state.token_manager.generate(id, "teacher").unwrap();
    let foreign = state.admin_token_manager.generate(id, "admin").unwrap();
    for token in [None, Some(foreign.as_str())] {
        assert_eq!(
            request(&state, "/api/v1/me/preferences", token, None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    sqlx::query("UPDATE users SET security_version=security_version+1 WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(
            &state,
            "/api/v1/me/preferences",
            Some(&valid),
            Some(json!({"interface_locale":"zh-Hans"}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn interface_preference_migration_is_reversible_only_without_saved_values(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261108010000_interface_preferences.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261108010000_interface_preferences.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let id = user(&pool, UserRole::Student).await;
    assert!(
        sqlx::query("UPDATE users SET interface_locale='zh-CN' WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query("UPDATE users SET interface_locale='zh-Hans' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261108010000_interface_preferences.down.sql"
        ))
        .execute(&mut *tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT interface_locale FROM users WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "zh-Hans"
    );
}

#[sqlx::test]
async fn legacy_migrator_cannot_restart_on_new_migration_ledger(pool: PgPool) {
    let all = sqlx::migrate!("./migrations");
    let old = sqlx::migrate::Migrator::with_migrations(
        all.iter()
            .filter(|m| m.version < 20261108010000)
            .cloned()
            .collect::<Vec<_>>(),
    );
    assert!(matches!(
        old.run(&pool).await,
        Err(sqlx::migrate::MigrateError::VersionMissing(20261108010000))
    ));
}
