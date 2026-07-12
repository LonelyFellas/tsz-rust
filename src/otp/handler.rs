use axum::{Json, extract::State};
use serde::Deserialize;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct SendOtpRequest {
    pub phone: Option<String>,
    pub email: Option<String>,
}

// TODO: 接 OtpService（下一步）——校验 phone/email、取 target+purpose、调 service.request、
// 映射 OtpServiceError→HTTP（RateLimited 429 / Store 503 fail-close / Send 503）。
/// POST /otp/send
pub async fn send_otp(State(_state): State<AppState>, Json(_req): Json<SendOtpRequest>) {}
