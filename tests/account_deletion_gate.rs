mod account_deletion_support;
use account_deletion_support::*;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Duration;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::{
    account_deletion::service,
    auth::extract::AuthUser,
    otp::model::Purpose,
    session::{repository::RefreshTokenRepository, service::SessionService},
    state::AppState,
};
const PASSWORD: &str = "Violet!River7294Cloud";
async fn password(pool: &PgPool, auth: &AuthUser) {
    let hash = tsz_rust::platform::Password::parse(PASSWORD)
        .unwrap()
        .hash()
        .await
        .unwrap();
    sqlx::query("UPDATE users SET password_hash=$2 WHERE id=$1")
        .bind(auth.subject)
        .bind(hash)
        .execute(pool)
        .await
        .unwrap();
}
async fn public_post(
    state: &AppState,
    path: &str,
    body: Value,
    cookie: Option<&str>,
) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("Content-Type", "application/json");
    if let Some(value) = cookie {
        req = req.header("Cookie", value);
    }
    tsz_rust::router(state.clone())
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
        .status()
}
#[sqlx::test]
async fn pending_login_refresh_and_old_access_work_until_database_deadline(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    password(&pool, &auth).await;
    let req = apply(&state, &auth, "0").await;
    let login = json!({"identifier":format!("{}@example.test",auth.subject),"password":PASSWORD});
    assert_eq!(
        public_post(&state, "/api/v1/auth/login", login.clone(), None).await,
        StatusCode::OK
    );
    let sessions = SessionService::new(
        RefreshTokenRepository::new(pool.clone()),
        Duration::days(30),
    );
    let issued = sessions.issue(auth.subject).await.unwrap();
    assert!(sessions.rotate(&issued.plaintext).await.is_ok());
    let old = sessions.issue(auth.subject).await.unwrap();
    deadline(&pool, req.id, 0).await;
    assert_eq!(
        public_post(&state, "/api/v1/auth/login", login, None).await,
        StatusCode::UNAUTHORIZED
    );
    let cookie = format!("refresh_token={}", old.plaintext);
    assert_eq!(
        public_post(&state, "/api/v1/auth/refresh", Value::Null, Some(&cookie)).await,
        StatusCode::UNAUTHORIZED
    );
    for path in [
        "/api/v1/me",
        "/api/v1/me/coins/wallet",
        "/api/v1/me/account-deletion",
    ] {
        assert_eq!(
            call(&state, &auth, "GET", path, Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert!(service::cancel(&state, &auth, req.id).await.is_err());
    let (otp_state, store) = AppState::for_test_with_otp_store(pool.clone());
    let target = format!("{}@example.test", auth.subject);
    let scope = tsz_rust::auth::security::login_code_target(&otp_state, &target)
        .await
        .unwrap();
    store
        .save_code(
            &scope,
            Purpose::Login,
            "123456",
            std::time::Duration::from_secs(300),
        )
        .await
        .unwrap();
    assert_eq!(
        public_post(
            &otp_state,
            "/api/v1/auth/login-otp",
            json!({"identifier":target,"code":"123456"}),
            None
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
}
#[sqlx::test]
async fn refresh_profile_security_and_cancel_recheck_after_waiting_past_deadline(pool: PgPool) {
    for action in ["refresh", "profile", "security", "cancel", "login"] {
        let (state, auth) = if action == "profile" {
            account_deletion_support::setup_bound(&pool).await
        } else {
            setup(&pool).await
        };
        password(&pool, &auth).await;
        let req = apply(&state, &auth, "0").await;
        let sessions = SessionService::new(
            RefreshTokenRepository::new(pool.clone()),
            Duration::days(30),
        );
        let issued = sessions.issue(auth.subject).await.unwrap();
        deadline(&pool, req.id, 1500).await;
        let mut gate = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
            .bind(auth.subject)
            .execute(&mut *gate)
            .await
            .unwrap();
        let owner = auth.subject;
        let action = action.to_owned();
        let kind = action.clone();
        let state2 = state.clone();
        let job = tokio::spawn(async move {
            let auth = AuthUser {
                subject: owner,
                role: "student".into(),
                security_version: 0,
            };
            match action.as_str() {
                "refresh" => {
                    let cookie = format!("refresh_token={}", issued.plaintext);
                    public_post(&state2, "/api/v1/auth/refresh", Value::Null, Some(&cookie)).await
                }
                "profile" => {
                    call(
                        &state2,
                        &auth,
                        "PATCH",
                        "/api/v1/me",
                        json!({"display_name":"Should not save"}),
                    )
                    .await
                    .0
                }
                "security" => call(
                    &state2,
                    &auth,
                    "POST",
                    "/api/v1/auth/password/change",
                    json!({"current_password":PASSWORD,"new_password":"Different!River7294Cloud"}),
                )
                .await
                .0,
                "cancel" => {
                    call(
                        &state2,
                        &auth,
                        "POST",
                        &format!("/api/v1/me/account-deletion/{}/cancel", req.id),
                        Value::Null,
                    )
                    .await
                    .0
                }
                _ => {
                    public_post(
                        &state2,
                        "/api/v1/auth/login",
                        json!({"identifier":format!("{owner}@example.test"),"password":PASSWORD}),
                        None,
                    )
                    .await
                }
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5),async{loop {
   let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')").fetch_one(&pool).await.unwrap();
   if waiting {break}tokio::time::sleep(std::time::Duration::from_millis(10)).await;
  }}).await.expect("request must reach user lock before deadline");
        // Wait on database wall time, without altering the pending request while it is blocked.
        sqlx::query("SELECT pg_sleep(GREATEST(0,EXTRACT(EPOCH FROM effective_at-clock_timestamp()))::double precision+0.02) FROM account_deletion_requests WHERE id=$1").bind(req.id).execute(&pool).await.unwrap();
        gate.commit().await.unwrap();
        assert_eq!(job.await.unwrap(), StatusCode::UNAUTHORIZED, "{kind}");
        let row:(String,String)=sqlx::query_as("SELECT u.display_name,d.status FROM users u JOIN account_deletion_requests d ON d.user_id=u.id WHERE d.id=$1").bind(req.id).fetch_one(&pool).await.unwrap();
        assert_eq!(row, ("Deletion".into(), "pending".into()));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM refresh_tokens WHERE user_id=$1")
                .bind(owner)
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }
}
