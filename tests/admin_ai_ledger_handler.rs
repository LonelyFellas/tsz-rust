use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, Request, StatusCode},
    routing::get,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex;
use tower::ServiceExt;
use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    question_bank::worker::AgentConfig,
    state::AppState,
};
use uuid::Uuid;

const URI: &str = "/api/v1/admin/ai-ledger";
const SERVICE_TOKEN: &str = "test-ledger-service-token-not-for-browser";

#[sqlx::test(migrations = "./migrations")]
async fn cost_endpoints_are_super_only_and_preserve_decimal_and_unknown_values(pool: PgPool) {
    let (state, mock, server) = with_upstream(pool).await;
    let (_, ordinary) = seed(&state, AdminRole::Admin).await;
    let (super_id, super_token) = seed(&state, AdminRole::SuperAdmin).await;
    for path in ["/costs", "/cost-overview"] {
        assert_eq!(
            request(&state, None, path).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(&state, Some(&ordinary), path).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    let summary = json!({"total_calls":2,"priced_calls":1,"unknown_calls":1,"estimated_cost_cny":"0.001392000000"});
    *mock.response.lock().await = (
        StatusCode::OK,
        json!({"ledger":payload(),"costs":[],"cost_summary":summary}),
    );
    let costs = request(&state, Some(&super_token), "/costs?page=2&page_size=20").await;
    assert_eq!(costs.0, StatusCode::OK);
    assert_eq!(costs.2["ledger"]["page"], 2);
    assert_eq!(
        costs.2["cost_summary"]["estimated_cost_cny"],
        "0.001392000000"
    );
    assert_eq!(costs.1["cache-control"], "no-store");
    let mut period = summary.clone();
    period["start"] = Value::Null;
    period["end"] = json!("2026-10-28T04:00:00Z");
    let mut unknown = period.clone();
    unknown["estimated_cost_cny"] = Value::Null;
    unknown["priced_calls"] = json!(0);
    unknown["unknown_calls"] = json!(2);
    let mut overview = json!({"generated_at":"2026-10-28T04:00:00Z","timezone":"Asia/Shanghai","currency":"CNY","pricing_version":"test-price","pricing_valid_until":"2027-01-01","total":period,"today":unknown,"week":period,"month":period});
    *mock.response.lock().await = (StatusCode::OK, overview.clone());
    let result = request(&state, Some(&super_token), "/cost-overview").await;
    assert_eq!(result.0, StatusCode::OK);
    assert!(result.2["today"]["estimated_cost_cny"].is_null());
    assert_eq!(result.2["total"]["estimated_cost_cny"], "0.001392000000");
    assert_eq!(result.1["cache-control"], "no-store");
    assert!(mock.queries.lock().await.last().unwrap().is_empty());
    overview["currency"] = json!("USD");
    *mock.response.lock().await = (StatusCode::OK, overview);
    assert_eq!(
        request(&state, Some(&super_token), "/cost-overview")
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(super_id)
        .execute(&state.pool)
        .await
        .unwrap();
    for path in ["/costs", "/cost-overview"] {
        assert_eq!(
            request(&state, Some(&super_token), path).await.0,
            StatusCode::FORBIDDEN
        );
    }
    server.abort();
}

async fn seed(state: &AppState, role: AdminRole) -> (Uuid, String) {
    let id = Uuid::now_v7();
    AdminRepository::new(state.pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("1{}", id.as_u128()),
            display_name: "账本测试".into(),
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
    query: &str,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().uri(format!("{URI}{query}"));
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
    (status, headers, serde_json::from_slice(&bytes).unwrap())
}

#[derive(Clone)]
struct Upstream {
    response: Arc<Mutex<(StatusCode, Value)>>,
    calls: Arc<AtomicUsize>,
    queries: Arc<Mutex<Vec<HashMap<String, String>>>>,
}
async fn upstream(
    State(state): State<Upstream>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(headers["authorization"], format!("Bearer {SERVICE_TOKEN}"));
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.queries.lock().await.push(query.clone());
    let (status, mut body) = state.response.lock().await.clone();
    let page = if body.get("ledger").is_some() {
        &mut body["ledger"]
    } else {
        &mut body
    };
    if page.is_object() && page.get("items").is_some() {
        assert!(query.contains_key("page") && query.contains_key("page_size"));
        page["page"] = json!(query["page"].parse::<u32>().unwrap());
        page["page_size"] = json!(query["page_size"].parse::<u32>().unwrap());
    }
    (status, Json(body))
}

fn payload() -> Value {
    json!({"items":[],"total":0,"page":1,"page_size":20,
      "summary":{"observed_calls":0,"usage_observed_calls":0,"cache_observed_calls":0,"input_tokens":null,"output_tokens":null,"cache_hit_tokens":null,"cache_miss_tokens":null,"cache_hit_rate":null,"total_duration_ms":null,"average_duration_ms":null}})
}

async fn with_upstream(pool: PgPool) -> (AppState, Upstream, tokio::task::JoinHandle<()>) {
    let mock = Upstream {
        response: Arc::new(Mutex::new((StatusCode::OK, payload()))),
        calls: Arc::new(AtomicUsize::new(0)),
        queries: Arc::new(Mutex::new(Vec::new())),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/model-calls", get(upstream))
        .route("/v1/model-costs", get(upstream))
        .route("/v1/model-costs/overview", get(upstream))
        .with_state(mock.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut state = AppState::for_test(pool);
    state.vocabulary_agent = Some(Arc::new(AgentConfig {
        base_url: format!("http://{address}"),
        outbound_token: SERVICE_TOKEN.into(),
        inbound_token: "unused-callback-token".into(),
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap(),
    }));
    (state, mock, server)
}

#[sqlx::test(migrations = "./migrations")]
async fn only_current_super_admin_can_read_ledger(pool: PgPool) {
    let (state, mock, server) = with_upstream(pool).await;
    let (ordinary, token) = seed(&state, AdminRole::Admin).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.access',$1)").bind(ordinary).execute(&state.pool).await.unwrap();
    // Even a stale/forged role claim cannot override the current database role.
    let elevated_claim = state
        .admin_token_manager
        .generate(ordinary, "super_admin")
        .unwrap();
    assert_eq!(request(&state, None, "").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        request(&state, Some(&token), "").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&state, Some(&elevated_claim), "").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    let (super_id, super_token) = seed(&state, AdminRole::SuperAdmin).await;
    let run_id = Uuid::now_v7();
    let result = request(
        &state,
        Some(&super_token),
        &format!("?page=2&page_size=50&run_id={run_id}"),
    )
    .await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1["cache-control"], "no-store");
    assert_eq!(result.2["page"], 2);
    assert_eq!(mock.queries.lock().await[0]["run_id"], run_id.to_string());
    assert!(result.2.get("private_debug").is_none());
    assert!(!result.2.to_string().contains(SERVICE_TOKEN));
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(super_id)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        request(&state, Some(&super_token), "").await.0,
        StatusCode::FORBIDDEN
    );
    sqlx::query(
        "UPDATE admins SET role='super_admin',security_version=security_version+1 WHERE id=$1",
    )
    .bind(super_id)
    .execute(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        request(&state, Some(&super_token), "").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[sqlx::test(migrations = "./migrations")]
async fn invalid_queries_and_upstream_errors_fail_closed(pool: PgPool) {
    let (state, mock, server) = with_upstream(pool).await;
    let (_, token) = seed(&state, AdminRole::SuperAdmin).await;
    for query in [
        "?page=0",
        "?page_size=101",
        "?run_id=invalid",
        "?admin=true",
    ] {
        assert_eq!(
            request(&state, Some(&token), query).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    for (status, body) in [
        (StatusCode::NOT_FOUND, json!({"detail":SERVICE_TOKEN})),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"detail":SERVICE_TOKEN}),
        ),
        (StatusCode::OK, json!({"invalid":SERVICE_TOKEN})),
        (StatusCode::OK, json!("x".repeat(1_000_001))),
    ] {
        *mock.response.lock().await = (status, body);
        let result = request(&state, Some(&token), "").await;
        assert_eq!(result.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(result.2["code"], "service_unavailable");
        assert!(!result.2.to_string().contains(SERVICE_TOKEN));
    }
    server.abort();
    assert_eq!(
        request(&state, Some(&token), "").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let mut missing = state.clone();
    missing.vocabulary_agent = None;
    assert_eq!(
        request(&missing, Some(&token), "").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
