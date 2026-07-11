use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// 领域错误。每个变体对应一类http结果。
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    #[error("forbidden")]
    Forbidden,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unauthenticated(String),

    /// 内部错误：真实原因藏在 anyhow 里，只进日志，不进响应。
    #[error("internal error")]
    Internal(#[source] anyhow::Error),
}

impl AppError {
    /// 把任意错误（sqlx::Error 等) 包成Internal错误。
    /// 为什么要这个构造器而不是 impl From: 写 `impl<E> From<E>` 会和标准库冲突
    pub fn internal<E: Into<anyhow::Error>>(e: E) -> Self {
        AppError::Internal(e.into())
    }

    /// 领域错误 -> HTTP 状态码。抽成纯函数，方便直接单侧
    fn status_code(&self) -> StatusCode {
        match self {
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
        }
    }
}

/// 统一响应体
#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();

        // 500 的真实原因只记日志，绝不写进响应体
        if let AppError::Internal(cause) = &self {
            tracing::error!(error = %cause, "internal server error");
        }

        // Internal 的 Display 固定是 "internal error"，天然隐藏 cause
        let body = ErrorBody {
            error: self.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt; // 读取响应体

    // —— 纯映射逻辑（同文件测试可访问私有 status_code）——

    #[test]
    fn status_codes_map_per_variant() {
        assert_eq!(AppError::NotFound.status_code(), StatusCode::NOT_FOUND);
        assert_eq!(
            AppError::BadRequest("x".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            AppError::Unauthenticated("x".into()).status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(AppError::Forbidden.status_code(), StatusCode::FORBIDDEN);
        assert_eq!(
            AppError::Conflict("x".into()).status_code(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            AppError::internal(anyhow::anyhow!("boom")).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn internal_hides_cause_in_message() {
        let err = AppError::internal(anyhow::anyhow!("db password leaked"));
        assert_eq!(
            err.to_string(),
            "internal error",
            "内部错误对外只暴露通用文案"
        );
    }

    #[test]
    fn bad_request_preserves_message() {
        let err = AppError::BadRequest("phone is required".into());
        assert_eq!(err.to_string(), "phone is required");
    }

    // —— 走 into_response 的端到端行为 ——

    #[tokio::test]
    async fn response_sets_status_and_json_body() {
        let resp = AppError::NotFound.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, serde_json::json!({ "error": "not found" }));
    }

    #[tokio::test]
    async fn internal_response_body_hides_cause() {
        let resp = AppError::internal(anyhow::anyhow!("secret cause")).into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "error": "internal error" }),
            "响应体不得泄露真实原因"
        );
    }
}
