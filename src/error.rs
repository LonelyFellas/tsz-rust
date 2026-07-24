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
    #[error("{message}")]
    ForbiddenCode { message: String, code: String },
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unauthenticated(String),
    #[error("too many requests")]
    TooManyRequests,
    #[error("service unavailable")]
    ServiceUnavailable,
    #[error("{0}")]
    Locked(String),

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
            AppError::NotFound => StatusCode::NOT_FOUND, // 404
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST, //400
            AppError::Forbidden => StatusCode::FORBIDDEN, // 403
            AppError::ForbiddenCode { .. } => StatusCode::FORBIDDEN, // 403
            AppError::Conflict(_) => StatusCode::CONFLICT, // 409
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR, // 500
            AppError::Unauthenticated(_) => StatusCode::UNAUTHORIZED, // 401
            // status_code() —— 这是 exhaustive match，不加会编译不过（好，漏不掉）
            AppError::TooManyRequests => StatusCode::TOO_MANY_REQUESTS, // 429
            AppError::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE, // 503
            AppError::Locked(_) => StatusCode::LOCKED,                  // 423
        }
    }
}

/// 统一响应体
#[derive(Serialize)]
struct ErrorBody {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();

        // 500 的真实原因只记日志，绝不写进响应体
        if let AppError::Internal(cause) = &self {
            tracing::error!(error = %cause, "internal server error");
        }

        // Internal 的 Display 固定是 "internal error"，天然隐藏 cause
        let code = match &self {
            AppError::ForbiddenCode { code, .. } => Some(code.clone()),
            _ => None,
        };
        let body = ErrorBody {
            error: self.to_string(),
            code,
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

    // —— admin 域扩展（admin-design.md §12）：Locked 423 + 带 code 的 403 ——

    #[test]
    fn locked_maps_to_423() {
        assert_eq!(
            AppError::Locked("x".into()).status_code(),
            StatusCode::LOCKED
        );
    }

    #[tokio::test]
    async fn locked_response_body_is_error_only() {
        // §12 锁定行的契约文案由调用方传入，error.rs 不硬编码业务文案
        let resp = AppError::Locked(
            "account temporarily locked due to too many failed login attempts".into(),
        )
        .into_response();
        assert_eq!(resp.status(), StatusCode::LOCKED);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "error": "account temporarily locked due to too many failed login attempts"
            }),
            "Locked 与其它变体同形：只有 error 键，不得多出 code"
        );
    }

    #[test]
    fn forbidden_code_maps_to_403() {
        assert_eq!(
            AppError::ForbiddenCode {
                message: "x".into(),
                code: "y".into(),
            }
            .status_code(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn forbidden_code_body_carries_code_field() {
        // 硬契约：前端靠 code=must_change_password 整页跳改密页（admin-design.md §7/§12）
        let resp = AppError::ForbiddenCode {
            message: "password change required".into(),
            code: "must_change_password".into(),
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "error": "password change required",
                "code": "must_change_password"
            }),
            "body 形状是前端路由依据，逐字节钉死"
        );
    }

    #[tokio::test]
    async fn plain_variants_do_not_leak_code_key() {
        // ErrorBody 若加 Option<code>，普通变体序列化时必须 skip——否则全站错误响应都多出 "code": null
        for err in [
            AppError::Forbidden,
            AppError::Unauthenticated("invalid credentials".into()),
            AppError::BadRequest("weak password".into()),
        ] {
            let resp = err.into_response();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert!(
                body.as_object().unwrap().get("code").is_none(),
                "无 code 语义的变体不得出现 code 键: {body}"
            );
        }
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
