use axum::{
    Router,
    routing::{get, patch, post},
};

use crate::{admin::accounts::handler, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(handler::list_admins).post(handler::create_admin))
        .route("/{admin_id}/status", patch(handler::set_admin_status))
        .route(
            "/{admin_id}/reset-password",
            post(handler::reset_admin_password),
        )
}
