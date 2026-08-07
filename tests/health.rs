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
    let resp = tsz_rust::router(tsz_rust::state::AppState::for_test(pool))
        .oneshot(
            Request::builder()
                .uri("/api-docs/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_uuid_v7_request_id(resp.headers());
}
