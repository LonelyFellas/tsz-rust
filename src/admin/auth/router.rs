use axum::{Router, routing::post};

use crate::{admin::auth::handler, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", post(handler::admin_login))
        .route("/change-password", post(handler::change_password))
        .route("/refresh", post(handler::admin_refresh))
        .route("/login-code", post(handler::admin_login_code))
        .route("/logout", post(handler::admin_logout))
        .route("/logout-all", post(handler::admin_logout_all))
}
