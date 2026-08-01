use axum::{Router, routing::get};

use crate::{
    admin::{self},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/profile", get(admin::profile::handler::admin_profile))
        .nest("/auth", admin::auth::router())
        .nest("/admins", admin::accounts::router())
}
