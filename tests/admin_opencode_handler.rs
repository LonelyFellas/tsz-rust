use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode},
    routing::{get, post},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Mutex;
use tower::ServiceExt;
use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin, opencode::OpenCodeConfig},
    state::AppState,
};
use uuid::Uuid;

const PASSWORD: &str = "test-opencode-server-password-never-for-browser";
const ATTEMPT: &str = "/login/attempt_123";

#[derive(Clone)]
struct Upstream {
    calls: Arc<AtomicUsize>,
    response: Arc<Mutex<(StatusCode, Value)>>,
}

async fn upstream(State(state): State<Upstream>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    use base64::Engine;
    assert_eq!(
        headers["authorization"],
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("opencode:{PASSWORD}"))
        )
    );
    state.calls.fetch_add(1, Ordering::SeqCst);
    let (status, value) = state.response.lock().await.clone();
    (status, Json(value))
}

async fn with_upstream(pool: PgPool) -> (AppState, Upstream, tokio::task::JoinHandle<()>) {
    let mock = Upstream {
        calls: Arc::new(AtomicUsize::new(0)),
        response: Arc::new(Mutex::new((
            StatusCode::OK,
            json!({"data":{
                "id":"opencode", "methods":[{"id":"device","type":"oauth"}], "connections":[]
            }}),
        ))),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/api/integration/opencode", get(upstream))
        .route("/api/integration/opencode/connect/oauth", post(upstream))
        .route(
            "/api/integration/opencode/connect/oauth/attempt_123",
            get(upstream).delete(upstream),
        )
        .with_state(mock.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut state = AppState::for_test(pool);
    state.opencode = Some(Arc::new(
        OpenCodeConfig::new(&format!("http://{address}"), PASSWORD.into()).unwrap(),
    ));
    (state, mock, server)
}

async fn seed(state: &AppState, role: AdminRole) -> (Uuid, String) {
    let id = Uuid::now_v7();
    AdminRepository::new(state.pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("1{}", id.as_u128()),
            display_name: "OpenCode 测试".into(),
            password_hash: "unused-hash".into(),
            role,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    (
        id,
        state
            .admin_token_manager
            .generate(id, role.as_str())
            .unwrap(),
    )
}

async fn request(
    state: &AppState,
    token: Option<&str>,
    method: Method,
    suffix: &str,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("/api/v1/admin/opencode{suffix}"));
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

#[sqlx::test(migrations = "./migrations")]
async fn login_is_super_only_and_projects_no_credentials(pool: PgPool) {
    let (state, mock, server) = with_upstream(pool).await;
    let (_, ordinary) = seed(&state, AdminRole::Admin).await;
    let (id, token) = seed(&state, AdminRole::SuperAdmin).await;
    let routes = [
        (Method::GET, ""),
        (Method::POST, "/login"),
        (Method::GET, ATTEMPT),
        (Method::DELETE, ATTEMPT),
    ];
    for (method, path) in &routes {
        assert_eq!(
            request(&state, None, method.clone(), path).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(&state, Some(&ordinary), method.clone(), path)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    let result = request(&state, Some(&token), Method::GET, "").await;
    assert_eq!(
        result.2,
        json!({"configured":true,"connected":false,"login_available":true})
    );
    assert_eq!(result.1["cache-control"], "no-store");
    *mock.response.lock().await = (
        StatusCode::OK,
        json!({"data":{
            "attemptID":"attempt_123", "url":"https://opencode.ai/console/device?user_code=TEST",
            "mode":"auto", "instructions":"upstream instructions", "time":{"expires":1793189558829_i64}, "access_token":"never-return"
        }}),
    );
    let result = request(&state, Some(&token), Method::POST, "/login").await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(
        result.2,
        json!({"attempt_id":"attempt_123","authorization_url":"https://opencode.ai/console/device?user_code=TEST","expires_at":"2026-10-28T12:12:38.829Z"})
    );
    assert_eq!(result.1["cache-control"], "no-store");
    for status in ["pending", "complete", "expired", "failed"] {
        *mock.response.lock().await = (
            StatusCode::OK,
            json!({"data":{"status":status,"time":{"expires":1793189558829_i64},"message":"private raw exception with secret"}}),
        );
        let result = request(&state, Some(&token), Method::GET, ATTEMPT).await;
        assert_eq!(result.0, StatusCode::OK);
        assert_eq!(result.2["status"], status);
        assert!(result.2.get("message").is_none());
    }
    *mock.response.lock().await = (StatusCode::NO_CONTENT, Value::Null);
    let result = request(&state, Some(&token), Method::DELETE, ATTEMPT).await;
    assert_eq!(result.0, StatusCode::NO_CONTENT);
    assert_eq!(result.1["cache-control"], "no-store");
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
    let calls = mock.calls.load(Ordering::SeqCst);
    for (method, path) in routes {
        assert_eq!(
            request(&state, Some(&token), method, path).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), calls);
    server.abort();
}

#[sqlx::test(migrations = "./migrations")]
async fn invalid_upstream_and_unsafe_links_fail_closed(pool: PgPool) {
    let (mut state, mock, server) = with_upstream(pool).await;
    let (_, token) = seed(&state, AdminRole::SuperAdmin).await;
    *mock.response.lock().await = (StatusCode::BAD_GATEWAY, json!({"secret":PASSWORD}));
    let result = request(&state, Some(&token), Method::GET, "").await;
    assert_eq!(result.0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!result.2.to_string().contains(PASSWORD));
    for url in [
        "javascript:alert(1)",
        "https://opencode.ai.evil.test/console/device",
        "https://opencode.ai@evil.test/console/device",
        "http://opencode.ai/console/device",
    ] {
        *mock.response.lock().await = (
            StatusCode::OK,
            json!({"data":{
                "attemptID":"attempt_123","url":url,"mode":"auto","time":{"expires":1793189558829_i64}
            }}),
        );
        assert_eq!(
            request(&state, Some(&token), Method::POST, "/login")
                .await
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    for value in [
        json!({"data":{}}),
        json!({"data":{"id":"other","methods":[],"connections":[]}}),
        json!({"data":{"id":"opencode","methods":[],"connections":{}}}),
    ] {
        *mock.response.lock().await = (StatusCode::OK, value);
        assert_eq!(
            request(&state, Some(&token), Method::GET, "").await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    let calls = mock.calls.load(Ordering::SeqCst);
    assert_eq!(
        request(&state, Some(&token), Method::GET, "/login/bad.id")
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), calls);
    state.opencode = None;
    let result = request(&state, Some(&token), Method::GET, "").await;
    assert_eq!(
        result.2,
        json!({"configured":false,"connected":false,"login_available":false})
    );
    assert_eq!(
        request(&state, Some(&token), Method::POST, "/login")
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    server.abort();
}

#[test]
fn server_config_rejects_remote_http_and_embedded_secrets() {
    for url in [
        "http://remote.test",
        "https://user:secret@example.test",
        "https://example.test?token=secret",
        "https://example.test/path",
    ] {
        assert!(OpenCodeConfig::new(url, PASSWORD.into()).is_err());
    }
    assert!(OpenCodeConfig::new("http://127.0.0.1:4096", PASSWORD.into()).is_ok());
    assert!(OpenCodeConfig::new("https://opencode.internal.test", PASSWORD.into()).is_ok());
    assert!(OpenCodeConfig::new("http://127.0.0.1:4096", "short".into()).is_err());
}
