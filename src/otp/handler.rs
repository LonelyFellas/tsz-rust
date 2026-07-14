use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;

use crate::{
    error::AppError,
    otp::{model::Purpose, service::OtpServiceError},
    state::AppState,
    user::service::normalize_identifier,
};

#[derive(Deserialize)]
pub struct SendOtpRequest {
    pub phone: Option<String>,
    pub email: Option<String>,
    pub purpose: Purpose,
}

// 映射 OtpServiceError→HTTP（RateLimited 429 / Store 503 fail-close / Send 503）。
/// POST /otp/send
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
    };

    state
        .otp_service
        .request(&target, req.purpose)
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
