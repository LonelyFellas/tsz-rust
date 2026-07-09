use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt; // oneshot
use tsz_rust::router; // ← 只有库 crate 导得进来，这就是拆 lib 的回报

#[tokio::test]
async fn healthz_returns_ok() {
    let resp = router()
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
