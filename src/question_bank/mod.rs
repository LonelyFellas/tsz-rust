mod algorithm;
pub mod dto;
pub mod handler;
pub(crate) mod learning;
mod source;
pub mod worker;

use crate::error::{AppError, ErrorCode};
fn invalid(message: &str) -> AppError {
    AppError::bad_request(ErrorCode::InvalidRequestBody, message)
}
fn conflict(message: &str) -> AppError {
    AppError::conflict(ErrorCode::LearningTaskConflict, None, message)
}
fn source_changed() -> AppError {
    AppError::conflict(
        ErrorCode::LearningRunUnavailable,
        None,
        "题目来源已改变，请重新生成",
    )
}
