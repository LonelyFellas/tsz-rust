use axum::{
    Router,
    routing::{get, patch},
};

use crate::{
    admin::{self},
    catalog, lexicon, speech,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/profile", get(admin::profile::handler::admin_profile))
        .route(
            "/profile/preferences",
            patch(admin::profile::handler::update_admin_preferences),
        )
        .route("/users", get(admin::accounts::handler::list_users))
        .route(
            "/users/{id}",
            get(admin::accounts::handler::get_user).patch(admin::accounts::handler::update_user),
        )
        .route(
            "/users/{id}/status",
            patch(admin::accounts::handler::set_user_status),
        )
        .nest("/auth", admin::auth::router())
        .nest("/admins", admin::accounts::router())
        .nest("/settings/parts-of-speech", catalog::router::router())
        .nest("/lexicon", lexicon::router::router())
        .nest("/speech", speech::preview::router::router())
}
