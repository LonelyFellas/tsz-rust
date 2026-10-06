use axum::{
    Router,
    routing::{get, patch, post},
};

use crate::{
    admin::{self},
    catalog, lexicon, speech,
    state::AppState,
};

pub fn router(_state: AppState) -> Router<AppState> {
    let business = Router::new()
        .nest("/settings/parts-of-speech", catalog::router::router())
        .nest("/settings/form-types", catalog::form_types::router())
        .nest("/lexicon", lexicon::router::router())
        .nest("/speech", speech::preview::router::router());

    Router::new()
        .route(
            "/coins/accounts",
            get(crate::coins::admin_handler::accounts),
        )
        .route(
            "/coins/accounts/{owner_type}/{owner_id}/entries",
            get(crate::coins::admin_handler::entries),
        )
        .route(
            "/coins/manual-credits",
            post(crate::coins::admin_handler::credit),
        )
        .route(
            "/coins/manual-credits/{operation_id}/reversal",
            post(crate::coins::admin_handler::reverse),
        )
        .route(
            "/coins/operations",
            get(crate::coins::admin_handler::operations),
        )
        .route("/me/coins/wallet", get(crate::coins::handler::admin_wallet))
        .route(
            "/me/coins/entries",
            get(crate::coins::handler::admin_entries),
        )
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
        .merge(admin::permissions::handler::router())
        .merge(business)
}
