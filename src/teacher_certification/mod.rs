pub mod cleanup;
pub mod files;
pub mod handler;
pub mod model;
mod service;

use crate::state::AppState;
use axum::{
    Router,
    routing::{delete, get, post},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/me/teacher-certification", get(handler::mine))
        .route("/api/v1/admin/teacher-applications", get(handler::list))
        .route(
            "/api/v1/me/teacher-certification/applications",
            post(handler::submit),
        )
        .route(
            "/api/v1/admin/teacher-applications/{id}/review",
            post(handler::review),
        )
        .route(
            "/api/v1/admin/users/{id}/teacher-certification",
            delete(handler::revoke),
        )
        .route("/api/v1/me/notifications", get(handler::notifications))
        .route(
            "/api/v1/me/teacher-certification/files",
            post(files::upload).layer(axum::extract::DefaultBodyLimit::max(files::MAX_FILE_BYTES)),
        )
        .route(
            "/api/v1/me/teacher-certification/files/{id}",
            get(files::read_own).delete(files::remove),
        )
        .route(
            "/api/v1/admin/teacher-certification/files/{id}",
            get(files::read_admin),
        )
        .route(
            "/api/v1/me/teacher-certification/applications/{id}",
            get(handler::own_detail),
        )
        .route(
            "/api/v1/admin/teacher-applications/{id}",
            get(handler::admin_detail),
        )
        .route(
            "/api/v1/me/notifications/{id}/read",
            axum::routing::patch(handler::read_notification),
        )
}
