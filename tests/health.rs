// tests/health.rs（项目根，不是 src/tests/）
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[sqlx::test]
async fn healthz_returns_ok(pool: sqlx::PgPool) {
    let resp = tsz_rust::router(pool)
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
