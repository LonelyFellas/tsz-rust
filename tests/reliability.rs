use axum::{Router, body::Body, extract::Request, http::StatusCode, middleware, routing::post};
use http_body_util::BodyExt;
use sqlx::{ConnectOptions, PgPool};
use tower::ServiceExt;
use tsz_rust::{error::AppError, platform, reliability, request_id::request_id_middleware};

#[sqlx::test]
async fn database_deadlines_rollback_writes_and_preserve_default_lock_budget(pool: PgPool) {
    sqlx::query("CREATE TABLE reliability_probe(id int primary key)")
        .execute(&pool)
        .await
        .unwrap();
    let runtime = platform::db::connect_runtime(pool.connect_options().to_url_lossy().as_str())
        .await
        .unwrap();
    let mut tx = runtime.begin().await.unwrap();
    sqlx::query("INSERT INTO reliability_probe VALUES(1)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let error = sqlx::query("SELECT pg_sleep(11)")
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert_eq!(
        AppError::internal(error).status_code(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    // PostgreSQL aborts the transaction even if the caller later tries COMMIT.
    let _ = tx.commit().await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM reliability_probe")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let mut tx = runtime.begin().await.unwrap();
    sqlx::query("SET LOCAL lock_timeout = DEFAULT")
        .execute(&mut *tx)
        .await
        .unwrap();
    let timeout: String = sqlx::query_scalar("SHOW lock_timeout")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(timeout, "3s");
    tx.rollback().await.unwrap();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE reliability_probe IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let error = sqlx::query("INSERT INTO reliability_probe VALUES(2)")
        .execute(&runtime)
        .await
        .unwrap_err();
    assert_eq!(
        tsz_rust::safe_log::error_kind(&error),
        "database_lock_timeout"
    );
    blocker.rollback().await.unwrap();
    runtime.close().await;
}

#[tokio::test]
async fn redis_command_response_has_a_budget_after_acquisition() {
    let pool = platform::connect_redis(&std::env::var("TEST_REDIS_URL").unwrap())
        .await
        .unwrap();
    let mut connection = pool.get().await.unwrap();
    let key = format!("reliability:{}", uuid::Uuid::now_v7());
    let start = std::time::Instant::now();
    let error = deadpool_redis::redis::cmd("BLPOP")
        .arg(key)
        .arg(5)
        .query_async::<Option<(String, String)>>(&mut connection)
        .await
        .unwrap_err();
    assert!(error.is_timeout());
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn stalled_request_body_and_panics_have_safe_problem_details_and_request_ids() {
    let app = Router::new()
        .route(
            "/body",
            post(|request: Request| async {
                let _ = request.into_body().collect().await;
                StatusCode::OK
            }),
        )
        .route(
            "/panic",
            post(|| async {
                panic!("injected-secret");
                #[allow(unreachable_code)]
                StatusCode::OK
            }),
        )
        .layer(middleware::from_fn(reliability::boundary))
        .layer(middleware::from_fn(request_id_middleware));
    let body = Body::from_stream(futures_util::stream::pending::<
        Result<String, std::io::Error>,
    >());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/body")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().contains_key("x-request-id"));
    let problem: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(problem["code"], "service_unavailable");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/panic")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(response.headers().contains_key("x-request-id"));
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!String::from_utf8_lossy(&body).contains("injected-secret"));
}
