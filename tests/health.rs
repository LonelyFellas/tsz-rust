// tests/health.rs（项目根，不是 src/tests/）
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header::HeaderValue};
use tower::ServiceExt;
use uuid::Uuid;

// These routes must work without live infrastructure. Dead lazy connections
// also make accidental database/Redis access fail instead of using a shared DB.
fn state_without_dependencies() -> tsz_rust::state::AppState {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(100))
        .connect_lazy("postgres://postgres@127.0.0.1:1/unused")
        .unwrap();
    let redis = deadpool_redis::Config::from_url("redis://127.0.0.1:1/0")
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap();
    tsz_rust::state::AppState::for_test_with_redis(pool, redis)
}

fn assert_uuid_v7_request_id(headers: &HeaderMap) {
    let request_id = headers
        .get("x-request-id")
        .and_then(|value: &HeaderValue| value.to_str().ok())
        .expect("all responses should include a valid x-request-id header");
    let request_id = Uuid::parse_str(request_id).expect("x-request-id should be a UUID");
    assert_eq!(request_id.get_version_num(), 7);
}

#[tokio::test]
async fn healthz_returns_ok() {
    let resp = tsz_rust::router(state_without_dependencies())
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_uuid_v7_request_id(resp.headers());
}

#[cfg(feature = "swagger")]
#[tokio::test]
async fn swagger_routes_use_the_global_request_id_middleware() {
    use http_body_util::BodyExt;

    let app = tsz_rust::router(state_without_dependencies());
    for (path, content_type) in [
        ("/api-docs/openapi.json", "application/json"),
        ("/swagger-ui/", "text/html"),
        ("/swagger-ui/swagger-ui.css", "text/css"),
        ("/swagger-ui/swagger-ui-bundle.js", "javascript"),
        ("/swagger-ui/swagger-initializer.js", "javascript"),
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        assert_uuid_v7_request_id(resp.headers());
        assert!(
            resp.headers()["content-type"]
                .to_str()
                .unwrap()
                .contains(content_type),
            "{path}"
        );
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(!body.is_empty(), "{path}");
        if path == "/api-docs/openapi.json" {
            let spec: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let snapshot: serde_json::Value =
                serde_json::from_str(include_str!("../docs/openapi.json")).unwrap();
            assert_eq!(spec, snapshot);
        }
        if path == "/swagger-ui/swagger-initializer.js" {
            assert!(String::from_utf8_lossy(&body).contains("/api-docs/openapi.json"));
        }
    }
}

#[tokio::test]
async fn separately_built_routers_keep_state_and_request_ids_independent() {
    use http_body_util::BodyExt;

    let chinese = tsz_rust::router(state_without_dependencies());
    let mut english_state = state_without_dependencies();
    english_state.interface_languages = tsz_rust::i18n::InterfaceLanguages::from_pairs(&[
        ("I18N_WEB_SUPPORTED_LOCALES".into(), "en-US".into()),
        ("I18N_WEB_DEFAULT_LOCALE".into(), "en-US".into()),
    ])
    .unwrap();
    let english = tsz_rust::router(english_state);
    let mut request_ids = std::collections::HashSet::new();

    for (app, locale) in [
        (&chinese, "zh-Hans"),
        (&english, "en-US"),
        (&chinese, "zh-Hans"),
        (&english, "en-US"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/i18n/capabilities?surface=web")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_uuid_v7_request_id(response.headers());
        assert!(request_ids.insert(response.headers()["x-request-id"].clone()));
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let capabilities: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(capabilities["default_locale"], locale);
        assert_eq!(
            capabilities["supported_locales"],
            serde_json::json!([locale])
        );
    }
}
