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
            "/wordlists/management",
            get(crate::wordlists::admin_handler::management_list),
        )
        .route(
            "/wordlists/catalog",
            get(crate::wordlists::admin_handler::catalog),
        )
        .route(
            "/wordlists/{id}",
            get(crate::wordlists::admin_handler::detail),
        )
        .route(
            "/wordlists/{id}/items",
            get(crate::wordlists::admin_handler::current_items),
        )
        .route(
            "/wordlists/{id}/visibility",
            post(crate::wordlists::admin_handler::visibility),
        )
        .route(
            "/wordlists/{id}/recommendation",
            post(crate::wordlists::admin_handler::recommend),
        )
        .route(
            "/wordlists/{id}/delete-impact",
            get(crate::wordlists::admin_handler::delete_impact),
        )
        .route(
            "/wordlists/{id}/delete",
            post(crate::wordlists::admin_handler::delete),
        )
        .route("/wordlists", get(crate::wordlists::admin_handler::list))
        .route(
            "/wordlists/{id}/review-requests",
            get(crate::wordlists::admin_handler::reviews),
        )
        .route(
            "/wordlists/{id}/review-requests/{request_id}/items",
            get(crate::wordlists::admin_handler::items),
        )
        .route(
            "/wordlists/{id}/review-requests/{request_id}/decision",
            post(crate::wordlists::admin_handler::decision),
        )
        .route(
            "/wordlists/{id}/withdraw",
            post(crate::wordlists::admin_handler::withdraw),
        )
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
        .route(
            "/users/{id}/coin-wallet",
            get(admin::accounts::user_resources::wallet),
        )
        .route(
            "/users/{id}/coin-entries",
            get(admin::accounts::user_resources::entries),
        )
        .route(
            "/users/{id}/identity-documents",
            get(admin::accounts::user_resources::identity_documents),
        )
        .route(
            "/users/{id}/identity-documents/{file_id}",
            get(admin::accounts::user_resources::identity_file),
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
