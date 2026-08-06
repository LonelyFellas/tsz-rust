use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Query, Request},
    http::{StatusCode, request::Parts},
};
use serde::de::DeserializeOwned;

use crate::error::{AppError, ErrorCode};

/// 统一 JSON rejection，避免 Axum 的默认文本响应绕过 API 错误契约。
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(request, state)
            .await
            .map_err(|rejection| {
                let status = rejection.status();
                let (code, message) = if status == StatusCode::BAD_REQUEST {
                    (ErrorCode::InvalidJson, "invalid JSON")
                } else {
                    (ErrorCode::InvalidRequestBody, "invalid request body")
                };
                AppError::request_error(code, message)
            })?;
        Ok(Self(value))
    }
}

/// 统一 query rejection，且不把框架或 serde 的内部报错文本直接暴露给客户端。
pub struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(value) = Query::<T>::from_request_parts(parts, state)
            .await
            .map_err(|_| {
                AppError::bad_request(ErrorCode::InvalidQuery, "invalid query parameters")
            })?;
        Ok(Self(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, routing::post};
    use http_body_util::BodyExt;
    use serde::Deserialize;
    use serde_json::Value;
    use tower::ServiceExt;

    #[derive(Deserialize)]
    struct Payload {
        value: String,
    }

    async fn json_handler(ApiJson(payload): ApiJson<Payload>) -> String {
        payload.value
    }

    async fn error_for(
        body: &'static str,
        content_type: Option<&'static str>,
    ) -> (StatusCode, Value) {
        let mut request = axum::http::Request::builder().method("POST").uri("/");
        if let Some(content_type) = content_type {
            request = request.header(axum::http::header::CONTENT_TYPE, content_type);
        }
        let response = Router::new()
            .route("/", post(json_handler))
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn malformed_json_is_400_invalid_json_problem() {
        let (status, body) = error_for("{", Some("application/json")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "invalid_json");
        assert_eq!(body["status"], 400);
        assert_eq!(body["type"], "urn:tsz:problem:invalid_json");
    }

    #[tokio::test]
    async fn dto_and_content_type_rejections_are_422_request_body_problems() {
        for (body, content_type) in [
            (r#"{"unknown":true}"#, Some("application/json")),
            (r#"{"value":1}"#, Some("application/json")),
            (r#"{"value":"ok"}"#, None),
        ] {
            let (status, body) = error_for(body, content_type).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(body["code"], "invalid_request_body");
            assert_eq!(body["status"], 422);
            assert!(body.get("error").is_none());
        }
    }
}
