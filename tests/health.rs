// tests/health.rs（项目根，不是 src/tests/）
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header::HeaderValue};
use tower::ServiceExt;
use uuid::Uuid;

fn assert_uuid_v7_request_id(headers: &HeaderMap) {
    let request_id = headers
        .get("x-request-id")
        .and_then(|value: &HeaderValue| value.to_str().ok())
        .expect("all responses should include a valid x-request-id header");
    let request_id = Uuid::parse_str(request_id).expect("x-request-id should be a UUID");
    assert_eq!(request_id.get_version_num(), 7);
}

#[sqlx::test]
async fn healthz_returns_ok(pool: sqlx::PgPool) {
    let resp = tsz_rust::router(tsz_rust::state::AppState::for_test(pool))
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
#[sqlx::test]
async fn swagger_routes_use_the_global_request_id_middleware(pool: sqlx::PgPool) {
    use http_body_util::BodyExt;

    let app = tsz_rust::router(tsz_rust::state::AppState::for_test(pool));
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
