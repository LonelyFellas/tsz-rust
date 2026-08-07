use std::fmt;

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct RequestId(Uuid);

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let request_id = RequestId(Uuid::now_v7());

    request.extensions_mut().insert(request_id);

    let mut response = next.run(request).await;
    response.headers_mut().insert(
        HeaderName::from_static("x-request-id"),
        HeaderValue::from_str(&request_id.to_string()).unwrap(),
    );

    response
}
