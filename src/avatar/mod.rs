pub mod cleanup;
pub mod dto;
pub mod handler;
pub mod image;
pub mod repository;
pub mod service;

use crate::state::AppState;
use axum::{
    Router,
    routing::{get, post},
};

pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/me/avatar/upload-url", post(handler::create_upload))
        .route("/api/v1/me/avatar", post(handler::confirm))
        .route("/api/v1/avatars/{id}", get(handler::read_public))
}
