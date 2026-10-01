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
        model::User,
        repository::UserRepository,
        service::{RegisterInput, UserService},
    },
};

async fn user(pool: &PgPool, phone: Option<&str>, email: Option<&str>) -> User {
    UserService::new(UserRepository::new(pool.clone()))
        .register(RegisterInput {
            phone: phone.map(str::to_owned),
            email: email.map(str::to_owned),
            password: "Original!River42Cloud".into(),
        })
        .await
        .unwrap()
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: Option<&str>,
    cookie: Option<&str>,
    body: Value,
) -> (StatusCode, Value, Option<String>) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/api/v1{path}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some(cookie) = cookie {
        req = req.header(header::COOKIE, cookie);
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        cookie,
    )
}

async fn post(
    state: &AppState,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value, Option<String>) {
    call(state, "POST", path, token, None, body).await
}

fn access(state: &AppState, user: &User) -> String {
    state
        .token_manager
        .generate_with_version(user.id, "student", user.security_version)
        .unwrap()
}

async fn binding_codes(state: &AppState, token: &str, contact: &str, channel: &str) {
    let (status, body, _) = post(
        state,
        "/me/contact/verification-code",
        Some(token),
        json!({"operation":"bind", "contact":contact, "verification_channel":channel}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, body, _) = post(
        state,
        "/me/contact/bind-code",
        Some(token),
        json!({"contact":contact}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

#[sqlx::test]
async fn bind_email_requires_both_proofs_and_revokes_sessions(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), None).await;
    let (s, login, cookie) = post(
        &state,
        "/auth/login",
        None,
        json!({"identifier":"13800138000","password":"Original!River42Cloud"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let token = login["access_token"].as_str().unwrap();
    binding_codes(&state, token, "Test@Example.com", "phone").await;
    let (s, body, clear) = post(&state, "/me/contact/bind", Some(token), json!({"contact":"test@example.com","code":"000000","verification_channel":"phone","verification_code":"000000"})).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{body}");
    let clear = clear.unwrap();
    for attribute in ["Max-Age=0", "Path=/api/v1/auth", "HttpOnly", "SameSite=Lax"] {
        assert!(clear.contains(attribute), "{clear}");
    }
    let saved = UserRepository::new(pool.clone())
        .get_by_id(&u.id)
        .await
        .unwrap();
    assert_eq!(saved.email.as_deref(), Some("test@example.com"));
    assert_eq!(saved.security_version, 1);
    assert_eq!(
        call(&state, "GET", "/auth/me", Some(token), None, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &state,
            "POST",
            "/auth/refresh",
            None,
            cookie.as_deref(),
            Value::Null
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (s, new_login, _) = post(
        &state,
        "/auth/login",
        None,
        json!({"identifier":"test@example.com","password":"Original!River42Cloud"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{new_login}");
    assert_eq!(
        call(
            &state,
            "GET",
            "/auth/me",
            new_login["access_token"].as_str(),
            None,
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[sqlx::test]
async fn binding_without_old_proof_and_cross_user_codes_fail(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let a = user(&pool, Some("13800138000"), None).await;
    let b = user(&pool, Some("13900139000"), None).await;
    let at = access(&state, &a);
    let bt = access(&state, &b);
    binding_codes(&state, &at, "new@example.com", "phone").await;
    let payload = json!({"contact":"new@example.com","code":"000000","verification_channel":"phone","verification_code":"000000"});
    assert_eq!(
        post(&state, "/me/contact/bind", Some(&bt), payload.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let mut wrong = payload;
    wrong["verification_code"] = json!("111111");
    assert_eq!(
        post(&state, "/me/contact/bind", Some(&at), wrong).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        UserRepository::new(pool)
            .get_by_id(&a.id)
            .await
            .unwrap()
            .email
            .is_none()
    );
}

#[sqlx::test]
async fn target_and_operation_are_part_of_the_old_proof(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), Some("old@example.com")).await;
    let token = access(&state, &u);
    binding_codes(&state, &token, "new@example.com", "phone").await;
    let (s,_,_)=post(&state,"/me/contact/bind",Some(&token),json!({"contact":"other@example.com","code":"000000","verification_channel":"phone","verification_code":"000000"})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = post(
        &state,
        "/me/contact/unbind",
        Some(&token),
        json!({"channel":"email","verification_channel":"phone","verification_code":"000000"}),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        UserRepository::new(pool)
            .get_by_id(&u.id)
            .await
            .unwrap()
            .email
            .as_deref(),
        Some("old@example.com")
    );
}

#[sqlx::test]
async fn only_contact_cannot_be_unbound_even_with_a_code(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, None, Some("only@example.com")).await;
    let token = access(&state, &u);
    for (path, body) in [
        (
            "/me/contact/verification-code",
            json!({"operation":"unbind","contact":"only@example.com","verification_channel":"email"}),
        ),
        (
            "/me/contact/unbind",
            json!({"channel":"email","verification_channel":"email","verification_code":"000000"}),
        ),
    ] {
        let (s, b, _) = post(&state, path, Some(&token), body).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    }
    assert_eq!(
        UserRepository::new(pool)
            .get_by_id(&u.id)
            .await
            .unwrap()
            .email
            .as_deref(),
        Some("only@example.com")
    );
}

#[sqlx::test]
async fn unbind_either_channel_and_concurrent_double_unbind_preserve_a_login(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), Some("old@example.com")).await;
    let token = access(&state, &u);
    for (contact, channel) in [("13800138000", "phone"), ("old@example.com", "email")] {
        assert_eq!(
            post(
                &state,
                "/me/contact/verification-code",
                Some(&token),
                json!({"operation":"unbind","contact":contact,"verification_channel":channel})
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
    }
    let (a, b) = tokio::join!(
        post(
            &state,
            "/me/contact/unbind",
            Some(&token),
            json!({"channel":"phone","verification_channel":"phone","verification_code":"000000"})
        ),
        post(
            &state,
            "/me/contact/unbind",
            Some(&token),
            json!({"channel":"email","verification_channel":"email","verification_code":"000000"})
        )
    );
    assert_eq!(
        [a.0, b.0]
            .iter()
            .filter(|s| **s == StatusCode::NO_CONTENT)
            .count(),
        1
    );
    assert!([a.0, b.0].contains(&StatusCode::UNAUTHORIZED));
    let saved = UserRepository::new(pool).get_by_id(&u.id).await.unwrap();
    assert!(saved.phone.is_some() ^ saved.email.is_some());
    assert_eq!(saved.security_version, 1);
}

#[sqlx::test]
async fn occupied_contact_is_conflict_and_preserves_account(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), None).await;
    let token = access(&state, &u);
    user(&pool, None, Some("occupied@example.com")).await;
    binding_codes(&state, &token, "occupied@example.com", "phone").await;
    let (s,b,_)=post(&state,"/me/contact/bind",Some(&token),json!({"contact":"occupied@example.com","code":"000000","verification_channel":"phone","verification_code":"000000"})).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "user_already_exists");
    let saved = UserRepository::new(pool).get_by_id(&u.id).await.unwrap();
    assert_eq!(saved.security_version, 0);
    assert!(saved.email.is_none());
}

#[sqlx::test]
async fn failed_new_code_does_not_change_contact_and_old_code_is_single_use(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), None).await;
    let token = access(&state, &u);
    binding_codes(&state, &token, "new@example.com", "phone").await;
    for code in ["111111", "000000"] {
        assert_eq!(post(&state,"/me/contact/bind",Some(&token),json!({"contact":"new@example.com","code":code,"verification_channel":"phone","verification_code":"000000"})).await.0,StatusCode::UNAUTHORIZED);
    }
    assert!(
        UserRepository::new(pool)
            .get_by_id(&u.id)
            .await
            .unwrap()
            .email
            .is_none()
    );
}

#[sqlx::test]
async fn rate_limit_is_shared_across_scopes_for_the_real_recipient(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), None).await;
    let token = access(&state, &u);
    assert_eq!(
        post(
            &state,
            "/me/contact/verification-code",
            Some(&token),
            json!({"operation":"bind","contact":"a@example.com","verification_channel":"phone"})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (s, b, _) = post(
        &state,
        "/me/contact/verification-code",
        Some(&token),
        json!({"operation":"bind","contact":"b@example.com","verification_channel":"phone"}),
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(b["code"], "otp_rate_limited");
}

#[sqlx::test]
async fn change_password_is_case_sensitive_and_invalidates_both_tokens(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), None).await;
    let (_, login, cookie) = post(
        &state,
        "/auth/login",
        None,
        json!({"identifier":"13800138000","password":"Original!River42Cloud"}),
    )
    .await;
    let token = login["access_token"].as_str().unwrap();
    assert_eq!(
        post(
            &state,
            "/auth/password/change",
            Some(token),
            json!({"current_password":"wrong","new_password":"Another!River73Cloud"})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (s, b, clear) = post(
        &state,
        "/auth/password/change",
        Some(token),
        json!({"current_password":"Original!River42Cloud","new_password":"Another!River73Cloud"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    assert!(clear.unwrap().contains("Max-Age=0"));
    assert_eq!(
        call(&state, "GET", "/auth/me", Some(token), None, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &state,
            "POST",
            "/auth/refresh",
            None,
            cookie.as_deref(),
            Value::Null
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            &state,
            "/auth/login",
            None,
            json!({"identifier":"13800138000","password":"Original!River42Cloud"})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            &state,
            "/auth/login",
            None,
            json!({"identifier":"13800138000","password":"Another!River73Cloud"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        UserRepository::new(pool)
            .get_by_id(&u.id)
            .await
            .unwrap()
            .security_version,
        1
    );
}

#[sqlx::test]
async fn password_policy_is_shared_and_unchanged_password_rejected(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), None).await;
    let token = access(&state, &u);
    for password in [
        "short1",
        "LettersOnly",
        "123456789012",
        "abcabcabcabcabcabc",
        "12345678901234567890",
    ] {
        assert_eq!(
            post(
                &state,
                "/auth/password/change",
                Some(&token),
                json!({"current_password":"Original!River42Cloud","new_password":password})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let (s, b, _) = post(
        &state,
        "/auth/password/change",
        Some(&token),
        json!({"current_password":"Original!River42Cloud","new_password":"Original!River42Cloud"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(b["code"], "password_unchanged");
}

#[sqlx::test]
async fn public_password_reset_codes_can_reset_phone_and_email_accounts(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    user(&pool, Some("13800138000"), None).await;
    user(&pool, None, Some("known@example.com")).await;
    for (field, identifier) in [("phone", "13800138000"), ("email", "KNOWN@example.com")] {
        let (status, body, _) = post(
            &state,
            "/otp/send",
            None,
            json!({field: identifier, "purpose": "password_reset"}),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let request = json!({
            "identifier": identifier,
            "code": "000000",
            "new_password": "Another!River73Cloud"
        });
        let (status, body, _) = post(&state, "/auth/password/reset", None, request.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            post(&state, "/auth/password/reset", None, request).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, body, _) = post(
            &state,
            "/auth/login",
            None,
            json!({"identifier": identifier, "password": "Another!River73Cloud"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}

#[sqlx::test]
async fn reset_has_uniform_forgot_responses_and_revokes_old_sessions(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, None, Some("known@example.com")).await;
    let token = access(&state, &u);
    for target in ["known@example.com", "unknown@example.com"] {
        let (s, b, _) = post(
            &state,
            "/auth/password/forgot",
            None,
            json!({"identifier":target}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b, json!({"status":"ok"}));
        assert_eq!(
            post(
                &state,
                "/auth/password/forgot",
                None,
                json!({"identifier":target})
            )
            .await
            .0,
            StatusCode::TOO_MANY_REQUESTS
        );
    }
    let body = json!({"identifier":"unknown@example.com","code":"000000","new_password":"Another!River73Cloud"});
    assert_eq!(
        post(&state, "/auth/password/reset", None, body).await.0,
        StatusCode::UNAUTHORIZED
    );
    let body = json!({"identifier":"KNOWN@example.com","code":"000000","new_password":"Another!River73Cloud"});
    assert_eq!(
        post(&state, "/auth/password/reset", None, body.clone())
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        post(&state, "/auth/password/reset", None, body).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&state, "GET", "/auth/me", Some(&token), None, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            &state,
            "/auth/login",
            None,
            json!({"identifier":"known@example.com","password":"Another!River73Cloud"})
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[sqlx::test]
async fn released_phone_does_not_transfer_an_old_login_code_to_a_new_account(pool: PgPool) {
    let (state, store) = AppState::for_test_with_otp_store(pool.clone());
    let old = user(&pool, Some("13800138000"), Some("old@example.com")).await;
    let token = access(&state, &old);
    let old_scope = tsz_rust::auth::security::login_code_target(&state, "13800138000")
        .await
        .unwrap();
    store
        .save_code(
            &old_scope,
            tsz_rust::otp::model::Purpose::Login,
            "123456",
            std::time::Duration::from_secs(300),
        )
        .await
        .unwrap();
    assert_eq!(
        post(
            &state,
            "/me/contact/verification-code",
            Some(&token),
            json!({"operation":"unbind","contact":"13800138000","verification_channel":"email"})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        post(
            &state,
            "/me/contact/unbind",
            Some(&token),
            json!({"channel":"phone","verification_channel":"email","verification_code":"000000"})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let new = user(&pool, Some("13800138000"), None).await;
    assert_ne!(new.id, old.id);
    let login = json!({"identifier":"13800138000","code":"123456"});
    assert_eq!(
        post(&state, "/auth/login-otp", None, login.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    let new_scope = tsz_rust::auth::security::login_code_target(&state, "13800138000")
        .await
        .unwrap();
    assert_ne!(old_scope, new_scope);
    store
        .save_code(
            &new_scope,
            tsz_rust::otp::model::Purpose::Login,
            "123456",
            std::time::Duration::from_secs(300),
        )
        .await
        .unwrap();
    let (status, body, _) = post(&state, "/auth/login-otp", None, login).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["id"], new.id.to_string());
}

#[sqlx::test]
async fn expired_and_wrong_purpose_reset_codes_cannot_change_password(pool: PgPool) {
    let (mut state, _) = AppState::for_test_with_otp_store(pool.clone());
    let u = user(&pool, None, Some("owner@example.com")).await;
    state.otp_service = std::sync::Arc::new(tsz_rust::otp::service::OtpService::new(
        tsz_rust::otp::store::OtpStore::with_prefix(
            state.redis.clone(),
            format!("security-expiry:{}:", uuid::Uuid::now_v7()),
        ),
        tsz_rust::otp::sender::OtpSender::Mock,
        std::time::Duration::ZERO,
        10,
        std::time::Duration::from_secs(1),
        5,
    ));
    assert_eq!(
        post(
            &state,
            "/otp/send",
            None,
            json!({"email":"owner@example.com","purpose":"login"})
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let body = json!({"identifier":"owner@example.com","code":"000000","new_password":"Another!River73Cloud"});
    assert_eq!(
        post(&state, "/auth/password/reset", None, body.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            &state,
            "/auth/password/forgot",
            None,
            json!({"identifier":"owner@example.com"})
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_eq!(
        post(&state, "/auth/password/reset", None, body).await.0,
        StatusCode::UNAUTHORIZED
    );
    let saved = UserRepository::new(pool).get_by_id(&u.id).await.unwrap();
    assert_eq!(saved.password_hash, u.password_hash);
}

#[sqlx::test]
async fn disabled_and_unknown_reset_targets_have_identical_public_responses(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let disabled = user(&pool, None, Some("disabled@example.com")).await;
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(disabled.id)
        .execute(&pool)
        .await
        .unwrap();
    let mut responses = Vec::new();
    for target in ["disabled@example.com", "unknown@example.com"] {
        let requested = post(
            &state,
            "/auth/password/forgot",
            None,
            json!({"identifier":target}),
        )
        .await;
        let reset = post(
            &state,
            "/auth/password/reset",
            None,
            json!({"identifier":target,"code":"000000","new_password":"Another!River73Cloud"}),
        )
        .await;
        responses.push((requested.0, requested.1, reset.0, reset.1));
    }
    assert_eq!(responses[0], responses[1]);
    assert_eq!(responses[0].0, StatusCode::OK);
    assert_eq!(responses[0].2, StatusCode::UNAUTHORIZED);
    assert_eq!(
        UserRepository::new(pool)
            .get_by_id(&disabled.id)
            .await
            .unwrap()
            .password_hash,
        disabled.password_hash
    );
}

#[sqlx::test]
async fn security_migration_cannot_be_rolled_back_after_credentials_change(pool: PgPool) {
    let u = user(&pool, Some("13800138000"), None).await;
    sqlx::query("UPDATE users SET security_version=1 WHERE id=$1")
        .bind(u.id)
        .execute(&pool)
        .await
        .unwrap();
    let error = tsz_rust::deployment_migrations::undo(&pool, 20260923050000, 20261001000000)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("cannot remove security_version"));
    let version: i64 = sqlx::query_scalar("SELECT security_version FROM users WHERE id=$1")
        .bind(u.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 1);
}

#[sqlx::test]
async fn legacy_access_tokens_work_only_until_a_security_change(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), None).await;
    let now = chrono::Utc::now().timestamp();
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &json!({"sub":u.id.to_string(),"aud":"web","role":"student","iat":now,"exp":now+900}),
        &jsonwebtoken::EncodingKey::from_secret(b"test-secret"),
    )
    .unwrap();
    assert_eq!(
        call(&state, "GET", "/auth/me", Some(&token), None, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE users SET security_version=1 WHERE id=$1")
        .bind(u.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, "GET", "/auth/me", Some(&token), None, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn change_password_clears_secure_cookie_at_the_auth_path(pool: PgPool) {
    let mut state = AppState::for_test(pool.clone());
    state.cookie_secure = true;
    let u = user(&pool, None, Some("owner@example.com")).await;
    let token = access(&state, &u);
    let (s, b, cookie) = post(
        &state,
        "/auth/password/change",
        Some(&token),
        json!({"current_password":"Original!River42Cloud","new_password":"Another!River73Cloud"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    let cookie = cookie.unwrap();
    for attribute in [
        "Secure",
        "HttpOnly",
        "SameSite=Lax",
        "Max-Age=0",
        "Path=/api/v1/auth",
    ] {
        assert!(cookie.contains(attribute), "{cookie}");
    }
}

#[sqlx::test]
async fn binding_phone_and_replacing_email_work_for_both_account_shapes(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, None, Some("owner@example.com")).await;
    let token = access(&state, &u);
    binding_codes(&state, &token, "13800138000", "email").await;
    assert_eq!(post(&state,"/me/contact/bind",Some(&token),json!({"contact":"13800138000","code":"000000","verification_channel":"email","verification_code":"000000"})).await.0,StatusCode::NO_CONTENT);
    let saved = UserRepository::new(pool.clone())
        .get_by_id(&u.id)
        .await
        .unwrap();
    let token = access(&state, &saved);
    let (mut state, _) = AppState::for_test_with_otp_store(pool.clone());
    state.cookie_secure = true;
    binding_codes(&state, &token, "replacement@example.com", "phone").await;
    let (s,b,cookie)=post(&state,"/me/contact/bind",Some(&token),json!({"contact":"replacement@example.com","code":"000000","verification_channel":"phone","verification_code":"000000"})).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    let cookie = cookie.unwrap();
    for attribute in [
        "Secure",
        "HttpOnly",
        "SameSite=Lax",
        "Max-Age=0",
        "Path=/api/v1/auth",
    ] {
        assert!(cookie.contains(attribute), "{cookie}");
    }
    let saved = UserRepository::new(pool).get_by_id(&u.id).await.unwrap();
    assert_eq!(saved.phone.as_deref(), Some("13800138000"));
    assert_eq!(saved.email.as_deref(), Some("replacement@example.com"));
    assert_eq!(saved.security_version, 2);
}

#[sqlx::test]
async fn refresh_waiting_on_user_lock_cannot_outlive_a_security_change(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), None).await;
    let (_, login, cookie) = post(
        &state,
        "/auth/login",
        None,
        json!({"identifier":"13800138000","password":"Original!River42Cloud"}),
    )
    .await;
    let old_token = login["access_token"].as_str().unwrap().to_owned();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(u.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let state2 = state.clone();
    let cookie = cookie.unwrap();
    let refresh = tokio::spawn(async move {
        call(
            &state2,
            "POST",
            "/auth/refresh",
            None,
            Some(&cookie),
            Value::Null,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let waiting: bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%FOR UPDATE OF u%')")
                .fetch_one(&pool).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("refresh should wait on the user lock");
    sqlx::query("UPDATE users SET security_version=security_version+1 WHERE id=$1")
        .bind(u.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE refresh_tokens SET revoked_at=NOW() WHERE user_id=$1")
        .bind(u.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(refresh.await.unwrap().0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        call(
            &state,
            "GET",
            "/auth/me",
            Some(&old_token),
            None,
            Value::Null
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM refresh_tokens WHERE user_id=$1 AND revoked_at IS NULL",
    )
    .bind(u.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live, 0);
}

#[sqlx::test]
async fn phone_password_reset_rejects_wrong_codes_then_accepts_the_normalized_password(
    pool: PgPool,
) {
    let state = AppState::for_test(pool.clone());
    user(&pool, Some("13800138000"), None).await;
    assert_eq!(
        post(
            &state,
            "/auth/password/forgot",
            None,
            json!({"identifier":"13800138000"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (s, b, _) = post(
        &state,
        "/auth/password/reset",
        None,
        json!({"identifier":"13800138000","code":"111111","new_password":"Another!River73Cloud"}),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(b["code"], "invalid_otp_code");
    assert_eq!(
        post(
            &state,
            "/auth/password/reset",
            None,
            json!({"identifier":"13800138000","code":"000000","new_password":"Another!River73Cloud"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        post(
            &state,
            "/auth/login",
            None,
            json!({"identifier":"13800138000","password":"Another!River73Cloud"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        post(
            &state,
            "/auth/login",
            None,
            json!({"identifier":"13800138000","password":"Original!River42Cloud"})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn authenticated_password_login_cannot_issue_a_session_after_a_concurrent_change(
    pool: PgPool,
) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), None).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(u.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let state2 = state.clone();
    let login = tokio::spawn(async move {
        post(
            &state2,
            "/auth/login",
            None,
            json!({"identifier":"13800138000","password":"Original!River42Cloud"}),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%SELECT security_version FROM users%FOR UPDATE%')").fetch_one(&pool).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("login should wait before issuing its session");
    sqlx::query("UPDATE users SET security_version=security_version+1 WHERE id=$1")
        .bind(u.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(login.await.unwrap().0, StatusCode::UNAUTHORIZED);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM refresh_tokens WHERE user_id=$1")
        .bind(u.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn session_write_failure_rolls_back_password_and_security_version(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let u = user(&pool, Some("13800138000"), None).await;
    let (_, login, _) = post(
        &state,
        "/auth/login",
        None,
        json!({"identifier":"13800138000","password":"Original!River42Cloud"}),
    )
    .await;
    let token = login["access_token"].as_str().unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_revoke() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test revoke failure'; END $$; CREATE TRIGGER reject_revoke BEFORE UPDATE ON refresh_tokens FOR EACH ROW EXECUTE FUNCTION reject_revoke();")
        .execute(&pool).await.unwrap();
    assert_eq!(
        post(
            &state,
            "/auth/password/change",
            Some(token),
            json!({"current_password":"Original!River42Cloud","new_password":"Another!River73Cloud"})
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let saved = UserRepository::new(pool).get_by_id(&u.id).await.unwrap();
    assert_eq!(saved.password_hash, u.password_hash);
    assert_eq!(saved.security_version, 0);
    assert_eq!(
        call(&state, "GET", "/auth/me", Some(token), None, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
}

#[sqlx::test]
async fn reset_checks_both_contacts_after_otp_proof_without_consuming_rejected_code(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    user(&pool, Some("13800138000"), Some("owner@example.com")).await;
    for (identifier, weak) in [
        ("owner@example.com", "Safe!13800138000Cloud"),
        ("13800138000", "Owner20261001!Cloud"),
    ] {
        assert_eq!(
            post(
                &state,
                "/auth/password/forgot",
                None,
                json!({"identifier":identifier})
            )
            .await
            .0,
            StatusCode::OK
        );
        // 没有有效证明，不能通过错误码探测另一联系方式。
        let invalid = post(
            &state,
            "/auth/password/reset",
            None,
            json!({"identifier":identifier,"code":"111111","new_password":weak}),
        )
        .await;
        assert_eq!(invalid.0, StatusCode::UNAUTHORIZED);
        assert_eq!(invalid.1["code"], "invalid_otp_code");
        let rejected = post(
            &state,
            "/auth/password/reset",
            None,
            json!({"identifier":identifier,"code":"000000","new_password":weak}),
        )
        .await;
        assert_eq!(rejected.0, StatusCode::BAD_REQUEST);
        assert_eq!(rejected.1["code"], "password_too_weak");
        assert_eq!(rejected.1["field"], "new_password");
        let valid = post(
            &state,
            "/auth/password/reset",
            None,
            json!({"identifier":identifier,"code":"000000","new_password":"Another!River73Cloud"}),
        )
        .await;
        assert_eq!(valid.0, StatusCode::OK);
        let replay = post(
            &state,
            "/auth/password/reset",
            None,
            json!({"identifier":identifier,"code":"000000","new_password":"Silver!River92Cloud"}),
        )
        .await;
        assert_eq!(replay.0, StatusCode::UNAUTHORIZED);
    }
}
