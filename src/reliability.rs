//! Request boundaries do not imply that a timed-out write was rolled back.
use crate::error::{AppError, ErrorCode};
use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::FutureExt;
use std::{panic::AssertUnwindSafe, time::Duration};

pub async fn boundary(request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|route| route.as_str())
        .unwrap_or("");
    let seconds = if route == "/readyz" {
        4
    } else if route.contains("/speech/")
        || route.contains("/audio")
        || route.contains("/avatar")
        || route.contains("/teacher-certification")
        || route.contains("/teacher-applications")
    {
        60
    } else {
        20
    };
    match tokio::time::timeout(
        Duration::from_secs(seconds),
        AssertUnwindSafe(next.run(request)).catch_unwind(),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => {
            tracing::error!(event = "http_panic");
            AppError::internal(anyhow::anyhow!("handler panic")).into_response()
        }
        Err(_) => {
            tracing::warn!(event = "http_timeout");
            AppError::unavailable(
                ErrorCode::ServiceUnavailable,
                "request timed out; outcome may be unknown",
            )
            .into_response()
        }
    }
}
