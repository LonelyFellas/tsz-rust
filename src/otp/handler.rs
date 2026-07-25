use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::{
    error::AppError,
    otp::{PublicOtpPurpose, service::OtpServiceError},
    state::AppState,
    user::service::normalize_identifier,
};

#[derive(Deserialize, ToSchema)]
pub struct SendOtpRequest {
    /// 手机号（与 email 二选一，恰好提供一个）
    #[schema(example = "13800138000")]
    pub phone: Option<String>,
    /// 邮箱（与 phone 二选一，恰好提供一个）
    pub email: Option<String>,
    // 注：purpose 是枚举 $ref，字段级 #[schema(example=...)] 会被 utoipa 丢弃，
    // 示例值放在 Purpose 枚举定义上（见 otp::model::Purpose）。
    pub purpose: PublicOtpPurpose,
}

// 映射 OtpServiceError→HTTP（RateLimited 429 / Store 503 fail-close / Send 503）。
/// POST /api/v1/otp/send
#[utoipa::path(
    post,
    path = "/api/v1/otp/send",
    tag = "otp",
    request_body = SendOtpRequest,
    responses(
        (status = 202, description = "验证码已受理并发送"),
        (status = 400, description = "手机号与邮箱须且仅须提供一个"),
        (status = 429, description = "请求过于频繁（冷却期内或超日限）"),
        (status = 503, description = "存储或发送上游不可用（fail-close）"),
    )
)]
pub async fn send_otp(
    State(state): State<AppState>,
    Json(req): Json<SendOtpRequest>,
) -> Result<StatusCode, AppError> {
    let target = match (
        req.phone
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        req.email
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        (Some(p), None) => normalize_identifier(p),
        (None, Some(e)) => normalize_identifier(e),
        _ => {
            return Err(AppError::BadRequest(
                "exactly one of phone or email is required".into(),
            ));
        }
    }
    .map_err(|e| AppError::BadRequest(e.to_string()))?;

    state
        .otp_service
        .request(&target, req.purpose.into())
        .await
        .map_err(map_otp_error)?;
    Ok(StatusCode::ACCEPTED) // 202
}

fn map_otp_error(e: OtpServiceError) -> AppError {
    match e {
        OtpServiceError::RateLimited => AppError::TooManyRequests, // 429
        OtpServiceError::Store(_) => AppError::ServiceUnavailable, // 503 fail-close，隐藏 cause
        OtpServiceError::Send(_) => AppError::ServiceUnavailable,  // 503 上游 provider 失败
        OtpServiceError::InvalidCode => {
            AppError::internal(anyhow::anyhow!("send 不该产生 InvalidCode"))
        } // 不可达，防御性
    }
}
