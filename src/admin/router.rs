use axum::{Router, routing::get};

use crate::{
    admin::{self},
    catalog, lexicon, speech,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/profile", get(admin::profile::handler::admin_profile))
        .route("/users", get(admin::accounts::handler::list_users))
        .nest("/auth", admin::auth::router())
        .nest("/admins", admin::accounts::router())
        .nest("/settings/parts-of-speech", catalog::router::router())
        .nest("/lexicon", lexicon::router::router())
        .nest("/speech", speech::preview::router::router())
}
