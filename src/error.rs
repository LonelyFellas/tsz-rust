use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::{error::Error, fmt};
use utoipa::ToSchema;

/// 对外稳定的机器错误码。前端应依据 code 分支，不能匹配展示文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    InvalidJson,
    InvalidRequestBody,
    InvalidQuery,
    InvalidPathParameter,
    InvalidPartOfSpeech,
    InvalidHeadword,
    UnsupportedLanguage,
    InvalidPhone,
    InvalidEmail,
    InvalidIdentifier,
    InvalidPassword,
    PasswordMissing,
    PasswordTooShort,
    PasswordTooLong,
    PasswordUnchanged,
    InvalidDisplayName,
    InvalidOtpCode,
    InvalidCredentials,
    InvalidToken,
    InvalidRefreshToken,
    UserNotFound,
    AdminNotFound,
    AccountDisabled,
    AccountLocked,
    MustChangePassword,
    Forbidden,
    OtpRateLimited,
    OtpUnavailable,
    PasswordHashUnavailable,
    UserAlreadyExists,
    PhoneAlreadyRegistered,
    PartOfSpeechNotFound,
    SubPartOfSpeechNotFound,
    PartOfSpeechConflict,
    SubPartOfSpeechConflict,
    RevisionConflict,
    ReferenceConflict,
    DetectionMismatch,
    DetectionExpired,
    DuplicateWord,
    IdempotencyConflict,
    StepNotReachable,
    ValidationFailed,
    DownstreamConfirmationRequired,
    EntryArchived,
    EntryHasInboundPublicationRefs,
    EntryHasUnavailablePublicationRefs,
    WordNotFound,
    PartOfSpeechInUse,
    SubPartOfSpeechInUse,
    ServiceUnavailable,
    InternalError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorDescriptor {
    pub slug: &'static str,
    pub title: &'static str,
    pub default_status: StatusCode,
}

impl ErrorCode {
    pub const ALL: [Self; 53] = [
        Self::NotFound,
        Self::InvalidJson,
        Self::InvalidRequestBody,
        Self::InvalidQuery,
        Self::InvalidPathParameter,
        Self::InvalidPartOfSpeech,
        Self::InvalidHeadword,
        Self::UnsupportedLanguage,
        Self::InvalidPhone,
        Self::InvalidEmail,
        Self::InvalidIdentifier,
        Self::InvalidPassword,
        Self::PasswordMissing,
        Self::PasswordTooShort,
        Self::PasswordTooLong,
        Self::PasswordUnchanged,
        Self::InvalidDisplayName,
        Self::InvalidOtpCode,
        Self::InvalidCredentials,
        Self::InvalidToken,
        Self::InvalidRefreshToken,
        Self::UserNotFound,
        Self::AdminNotFound,
        Self::AccountDisabled,
        Self::AccountLocked,
        Self::MustChangePassword,
        Self::Forbidden,
        Self::OtpRateLimited,
        Self::OtpUnavailable,
        Self::PasswordHashUnavailable,
        Self::UserAlreadyExists,
        Self::PhoneAlreadyRegistered,
        Self::PartOfSpeechNotFound,
        Self::SubPartOfSpeechNotFound,
        Self::PartOfSpeechConflict,
        Self::SubPartOfSpeechConflict,
        Self::RevisionConflict,
        Self::ReferenceConflict,
        Self::DetectionMismatch,
        Self::DetectionExpired,
        Self::DuplicateWord,
        Self::IdempotencyConflict,
        Self::StepNotReachable,
        Self::ValidationFailed,
        Self::DownstreamConfirmationRequired,
        Self::EntryArchived,
        Self::EntryHasInboundPublicationRefs,
        Self::EntryHasUnavailablePublicationRefs,
        Self::WordNotFound,
        Self::PartOfSpeechInUse,
        Self::SubPartOfSpeechInUse,
        Self::ServiceUnavailable,
        Self::InternalError,
    ];

    pub const fn descriptor(self) -> ErrorDescriptor {
        let (slug, title, default_status) = match self {
            Self::NotFound => ("not_found", "Resource not found", StatusCode::NOT_FOUND),
            Self::InvalidJson => ("invalid_json", "Invalid JSON", StatusCode::BAD_REQUEST),
            Self::InvalidRequestBody => (
                "invalid_request_body",
                "Invalid request body",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            Self::InvalidQuery => ("invalid_query", "Invalid query", StatusCode::BAD_REQUEST),
            Self::InvalidPathParameter => (
                "invalid_path_parameter",
                "Invalid path parameter",
                StatusCode::BAD_REQUEST,
            ),
            Self::InvalidPartOfSpeech => (
                "invalid_part_of_speech",
                "Invalid part of speech",
                StatusCode::BAD_REQUEST,
            ),
            Self::InvalidHeadword => (
                "invalid_headword",
                "Invalid headword",
                StatusCode::BAD_REQUEST,
            ),
            Self::UnsupportedLanguage => (
                "unsupported_language",
                "Unsupported language",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            Self::InvalidPhone => ("invalid_phone", "Invalid phone", StatusCode::BAD_REQUEST),
            Self::InvalidEmail => ("invalid_email", "Invalid email", StatusCode::BAD_REQUEST),
            Self::InvalidIdentifier => (
                "invalid_identifier",
                "Invalid identifier",
                StatusCode::BAD_REQUEST,
            ),
            Self::InvalidPassword => (
                "invalid_password",
                "Invalid password",
                StatusCode::BAD_REQUEST,
            ),
            Self::PasswordMissing => (
                "password_missing",
                "Password missing",
                StatusCode::BAD_REQUEST,
            ),
            Self::PasswordTooShort => (
                "password_too_short",
                "Password too short",
                StatusCode::BAD_REQUEST,
            ),
            Self::PasswordTooLong => (
                "password_too_long",
                "Password too long",
                StatusCode::BAD_REQUEST,
            ),
            Self::PasswordUnchanged => (
                "password_unchanged",
                "Password unchanged",
                StatusCode::BAD_REQUEST,
            ),
            Self::InvalidDisplayName => (
                "invalid_display_name",
                "Invalid display name",
                StatusCode::BAD_REQUEST,
            ),
            Self::InvalidOtpCode => (
                "invalid_otp_code",
                "Invalid verification code",
                StatusCode::UNAUTHORIZED,
            ),
            Self::InvalidCredentials => (
                "invalid_credentials",
                "Invalid credentials",
                StatusCode::UNAUTHORIZED,
            ),
            Self::InvalidToken => ("invalid_token", "Invalid token", StatusCode::UNAUTHORIZED),
            Self::InvalidRefreshToken => (
                "invalid_refresh_token",
                "Invalid refresh token",
                StatusCode::UNAUTHORIZED,
            ),
            Self::UserNotFound => ("user_not_found", "User not found", StatusCode::UNAUTHORIZED),
            Self::AdminNotFound => (
                "admin_not_found",
                "Administrator not found",
                StatusCode::UNAUTHORIZED,
            ),
            Self::AccountDisabled => (
                "account_disabled",
                "Account disabled",
                StatusCode::FORBIDDEN,
            ),
            Self::AccountLocked => ("account_locked", "Account locked", StatusCode::LOCKED),
            Self::MustChangePassword => (
                "must_change_password",
                "Password change required",
                StatusCode::FORBIDDEN,
            ),
            Self::Forbidden => ("forbidden", "Forbidden", StatusCode::FORBIDDEN),
            Self::OtpRateLimited => (
                "otp_rate_limited",
                "Too many verification attempts",
                StatusCode::TOO_MANY_REQUESTS,
            ),
            Self::OtpUnavailable => (
                "otp_unavailable",
                "Verification service unavailable",
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            Self::PasswordHashUnavailable => (
                "password_hash_unavailable",
                "Password service unavailable",
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            Self::UserAlreadyExists => (
                "user_already_exists",
                "User already exists",
                StatusCode::CONFLICT,
            ),
            Self::PhoneAlreadyRegistered => (
                "phone_already_registered",
                "Phone already registered",
                StatusCode::CONFLICT,
            ),
            Self::PartOfSpeechNotFound => (
                "part_of_speech_not_found",
                "Part of speech not found",
                StatusCode::NOT_FOUND,
            ),
            Self::SubPartOfSpeechNotFound => (
                "sub_part_of_speech_not_found",
                "Sub part of speech not found",
                StatusCode::NOT_FOUND,
            ),
            Self::PartOfSpeechConflict => (
                "part_of_speech_conflict",
                "Part of speech conflict",
                StatusCode::CONFLICT,
            ),
            Self::SubPartOfSpeechConflict => (
                "sub_part_of_speech_conflict",
                "Sub part of speech conflict",
                StatusCode::CONFLICT,
            ),
            Self::RevisionConflict => (
                "revision_conflict",
                "Revision conflict",
                StatusCode::CONFLICT,
            ),
            Self::ReferenceConflict => (
                "reference_conflict",
                "Referenced publication changed",
                StatusCode::CONFLICT,
            ),
            Self::DetectionMismatch => (
                "detection_mismatch",
                "Detection does not match",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            Self::DetectionExpired => ("detection_expired", "Detection expired", StatusCode::GONE),
            Self::DuplicateWord => (
                "duplicate_word",
                "Word already exists",
                StatusCode::CONFLICT,
            ),
            Self::IdempotencyConflict => (
                "idempotency_conflict",
                "Idempotency key conflict",
                StatusCode::CONFLICT,
            ),
            Self::StepNotReachable => (
                "step_not_reachable",
                "Step not reachable",
                StatusCode::CONFLICT,
            ),
            Self::ValidationFailed => (
                "validation_failed",
                "Validation failed",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            Self::DownstreamConfirmationRequired => (
                "downstream_confirmation_required",
                "Downstream confirmation required",
                StatusCode::CONFLICT,
            ),
            Self::EntryArchived => ("entry_archived", "Entry archived", StatusCode::CONFLICT),
            Self::EntryHasInboundPublicationRefs => (
                "entry_has_inbound_publication_refs",
                "Entry has inbound publication references",
                StatusCode::CONFLICT,
            ),
            Self::EntryHasUnavailablePublicationRefs => (
                "entry_has_unavailable_publication_refs",
                "Entry has unavailable publication references",
                StatusCode::CONFLICT,
            ),
            Self::WordNotFound => ("word_not_found", "Word not found", StatusCode::NOT_FOUND),
            Self::PartOfSpeechInUse => (
                "part_of_speech_in_use",
                "Part of speech in use",
                StatusCode::CONFLICT,
            ),
            Self::SubPartOfSpeechInUse => (
                "sub_part_of_speech_in_use",
                "Sub part of speech in use",
                StatusCode::CONFLICT,
            ),
            Self::ServiceUnavailable => (
                "service_unavailable",
                "Service unavailable",
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            Self::InternalError => (
                "internal_error",
                "Internal server error",
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        };
        ErrorDescriptor {
            slug,
            title,
            default_status,
        }
    }

    pub fn type_uri(self) -> String {
        format!("urn:tsz:problem:{}", self.descriptor().slug)
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProblemDetails {
    /// RFC 9457 问题类型；同一 code 永久映射到同一 URI。
    #[serde(rename = "type")]
    #[schema(rename = "type", example = "urn:tsz:problem:invalid_phone")]
    pub type_uri: String,
    /// 类型级稳定短标题；客户端不得据此分支。
    pub title: &'static str,
    /// 与 HTTP 状态行一致。
    pub status: u16,
    /// 本次错误的安全说明；客户端不得据此分支。
    pub detail: String,
    #[schema(example = "invalid_phone")]
    pub code: ErrorCode,
    /// 表单错误对应的请求字段；非字段错误省略。
    #[schema(example = "phone")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<&'static str>,
    /// 多字段/多节点校验问题；智能词库等复杂表单按稳定 node_id 定位。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub field_issues: Option<Vec<serde_json::Value>>,
    /// 领域错误的结构化上下文；客户端不得解析 detail 文案。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub meta: Option<ProblemMeta>,
}

#[derive(Debug, Default, Serialize, ToSchema)]
pub struct ProblemMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub current_revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub current_lifecycle_revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub usage_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub part_of_speech_id: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub word_id: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub max_reachable_step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub affected_node_ids: Option<Vec<uuid::Uuid>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub reference_locations: Option<Vec<ProblemReferenceLocation>>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProblemReferenceLocation {
    pub target_sense_id: uuid::Uuid,
    pub source_entry_id: uuid::Uuid,
    pub source_publication_id: uuid::Uuid,
    pub source_node_id: uuid::Uuid,
    pub reference_kind: String,
}

#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    response: ProblemDetails,
    source: Option<anyhow::Error>,
}

impl AppError {
    fn new(
        status: StatusCode,
        code: ErrorCode,
        message: impl Into<String>,
        field: Option<&'static str>,
    ) -> Self {
        let descriptor = code.descriptor();
        Self {
            status,
            response: ProblemDetails {
                type_uri: code.type_uri(),
                title: descriptor.title,
                status: status.as_u16(),
                detail: message.into(),
                code,
                field,
                field_issues: None,
                meta: None,
            },
            source: None,
        }
    }

    pub(crate) fn request_error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code.descriptor().default_status, code, message, None)
    }

    pub fn bad_request(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message, None)
    }

    pub fn validation(code: ErrorCode, field: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message, Some(field))
    }

    pub fn unprocessable(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message, None)
    }

    pub fn unauthorized(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message, None)
    }

    pub fn forbidden(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message, None)
    }

    pub fn conflict(
        code: ErrorCode,
        field: Option<&'static str>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(StatusCode::CONFLICT, code, message, field)
    }

    pub fn gone(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(StatusCode::GONE, code, message, None)
    }

    pub fn locked(message: impl Into<String>) -> Self {
        Self::new(StatusCode::LOCKED, ErrorCode::AccountLocked, message, None)
    }

    pub fn rate_limited(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, code, message, None)
    }

    pub fn unavailable(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, code, message, None)
    }

    pub fn unavailable_with_source<E: Into<anyhow::Error>>(
        code: ErrorCode,
        message: impl Into<String>,
        source: E,
    ) -> Self {
        let mut error = Self::unavailable(code, message);
        error.source = Some(source.into());
        error
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message, None)
    }

    pub fn not_found_with_code(code: ErrorCode, message: impl Into<String>) -> Self {
        debug_assert_eq!(code.descriptor().default_status, StatusCode::NOT_FOUND);
        Self::new(StatusCode::NOT_FOUND, code, message, None)
    }

    pub fn with_meta(mut self, meta: ProblemMeta) -> Self {
        self.response.meta = Some(meta);
        self
    }

    pub fn with_field_issues<T: Serialize>(mut self, issues: &[T]) -> Self {
        self.response.field_issues = Some(
            issues
                .iter()
                .filter_map(|issue| serde_json::to_value(issue).ok())
                .collect(),
        );
        self
    }

    /// 内部原因仅写服务端日志，对外固定为 internal_error。
    pub fn internal<E: Into<anyhow::Error>>(source: E) -> Self {
        let mut error = Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::InternalError,
            "internal error",
            None,
        );
        error.source = Some(source.into());
        error
    }

    pub fn status_code(&self) -> StatusCode {
        self.status
    }

    pub fn code(&self) -> ErrorCode {
        self.response.code
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.response.detail)
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(|source| source.as_ref())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if let Some(source) = &self.source {
            tracing::error!(
                error = %source,
                status = %self.status,
                code = ?self.response.code,
                "request failed"
            );
        }
        let mut response = (self.status, Json(self.response)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/problem+json"),
        );
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header;
    use http_body_util::BodyExt;
    use serde_json::json;
    use std::collections::HashSet;

    async fn response_json(error: AppError) -> (StatusCode, serde_json::Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn validation_error_has_stable_code_and_field() {
        let (status, body) = response_json(AppError::validation(
            ErrorCode::InvalidPhone,
            "phone",
            "invalid phone",
        ))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body,
            json!({
                "type":"urn:tsz:problem:invalid_phone",
                "title":"Invalid phone",
                "status":400,
                "detail":"invalid phone",
                "code":"invalid_phone",
                "field":"phone"
            })
        );
    }

    #[tokio::test]
    async fn non_field_error_omits_field() {
        let (_, body) = response_json(AppError::unauthorized(
            ErrorCode::InvalidCredentials,
            "invalid credentials",
        ))
        .await;
        assert_eq!(
            body,
            json!({
                "type":"urn:tsz:problem:invalid_credentials",
                "title":"Invalid credentials",
                "status":401,
                "detail":"invalid credentials",
                "code":"invalid_credentials"
            })
        );
    }

    #[tokio::test]
    async fn internal_error_hides_source() {
        let (status, body) =
            response_json(AppError::internal(anyhow::anyhow!("database password"))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({
                "type":"urn:tsz:problem:internal_error",
                "title":"Internal server error",
                "status":500,
                "detail":"internal error",
                "code":"internal_error"
            })
        );
        assert!(!body.to_string().contains("database password"));
    }

    #[test]
    fn display_never_exposes_internal_source() {
        let error = AppError::internal(anyhow::anyhow!("secret"));
        assert_eq!(error.to_string(), "internal error");
    }

    #[test]
    fn descriptors_are_complete_unique_and_consistent() {
        let mut slugs = HashSet::new();
        let mut type_uris = HashSet::new();

        for code in ErrorCode::ALL {
            let descriptor = code.descriptor();
            let serialized = serde_json::to_value(code).unwrap();
            assert_eq!(serialized, descriptor.slug);
            assert!(!descriptor.title.trim().is_empty());
            assert!(
                slugs.insert(descriptor.slug),
                "duplicate slug: {}",
                descriptor.slug
            );
            assert!(
                type_uris.insert(code.type_uri()),
                "duplicate type URI for {code:?}"
            );
            assert!((400..600).contains(&descriptor.default_status.as_u16()));
        }
    }

    #[tokio::test]
    async fn every_descriptor_default_status_matches_http_and_body_status() {
        for code in ErrorCode::ALL {
            let expected = code.descriptor().default_status;
            let (status, body) = response_json(AppError::request_error(code, "safe detail")).await;
            assert_eq!(status, expected, "default HTTP status for {code:?}");
            assert_eq!(
                body["status"],
                expected.as_u16(),
                "body status for {code:?}"
            );
            assert_eq!(body["type"], code.type_uri(), "type URI for {code:?}");
        }
    }

    #[tokio::test]
    async fn status_body_and_problem_content_type_are_consistent() {
        let response =
            AppError::unprocessable(ErrorCode::InvalidRequestBody, "invalid request body")
                .into_response();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["status"], 422);
        assert_eq!(body["code"], "invalid_request_body");
        assert!(body.get("error").is_none());
    }
}
