use axum::{
    Json,
    extract::{
        FromRequest, FromRequestParts, Path, Query, Request, path::ErrorKind as PathErrorKind,
        rejection::PathRejection,
    },
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
                // 超出 body limit 的 413 必须自成一档：请求体结构合法、只是太大，
                // 混进 invalid_request_body 会让前端把「录太多」误报成「格式错」。
                let (code, message) = if status == StatusCode::BAD_REQUEST {
                    (ErrorCode::InvalidJson, "invalid JSON")
                } else if status == StatusCode::PAYLOAD_TOO_LARGE {
                    (ErrorCode::PayloadTooLarge, "request body is too large")
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

pub struct ApiPath<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Path(value) = Path::<T>::from_request_parts(parts, state)
            .await
            .map_err(path_rejection)?;

        Ok(Self(value))
    }
}

fn path_rejection(rejection: PathRejection) -> AppError {
    let field = match &rejection {
        PathRejection::FailedToDeserializePathParams(error) => match error.kind() {
            PathErrorKind::ParseErrorAtKey { key, .. }
            | PathErrorKind::DeserializeError { key, .. }
            | PathErrorKind::InvalidUtf8InPathParam { key } => known_path_field(key),
            _ => None,
        },
        _ => None,
    };

    match field {
        Some(field) => AppError::validation(
            ErrorCode::InvalidPathParameter,
            field,
            "invalid path parameter",
        ),
        None => AppError::bad_request(ErrorCode::InvalidPathParameter, "invalid path parameter"),
    }
}

fn known_path_field(field: &str) -> Option<&'static str> {
    match field {
        "id" => Some("id"),
        "sub_id" => Some("sub_id"),
        _ => None,
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

    async fn error_for(body: &str, content_type: Option<&'static str>) -> (StatusCode, Value) {
        error_for_body(Body::from(body.to_owned()), content_type, None).await
    }

    async fn error_for_body(
        body: Body,
        content_type: Option<&'static str>,
        body_limit: Option<usize>,
    ) -> (StatusCode, Value) {
        let mut request = axum::http::Request::builder().method("POST").uri("/");
        if let Some(content_type) = content_type {
            request = request.header(axum::http::header::CONTENT_TYPE, content_type);
        }
        let mut route = post(json_handler);
        if let Some(limit) = body_limit {
            route = route.layer(axum::extract::DefaultBodyLimit::max(limit));
        }
        let response = Router::new()
            .route("/", route)
            .oneshot(request.body(body).unwrap())
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

    // 超出 body limit 的请求体本身是合法 JSON，只能报 413，不能退化成 422 invalid_request_body。
    #[tokio::test]
    async fn oversized_body_is_413_payload_too_large_problem() {
        let payload = format!(r#"{{"value":"{}"}}"#, "a".repeat(256));
        let (status, body) =
            error_for_body(Body::from(payload), Some("application/json"), Some(64)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["code"], "payload_too_large");
        assert_eq!(body["status"], 413);
        assert_eq!(body["type"], "urn:tsz:problem:payload_too_large");
    }
}
