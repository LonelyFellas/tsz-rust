use axum::{Router, routing::get};

use crate::state::AppState;

use super::handler;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/voices", get(handler::list_voices))
        .route("/previews", axum::routing::post(handler::create_preview))
        .route(
            "/audio",
            axum::routing::post(crate::speech::generated::generate),
        )
        .route("/library", get(crate::speech::library::handler::list))
        .route(
            "/groups",
            get(crate::speech::library::handler::groups)
                .put(crate::speech::library::handler::save_groups),
        )
        .route(
            "/library/{alias}/personal",
            axum::routing::put(crate::speech::library::handler::save_settings),
        )
        .route(
            "/library/{alias}/description",
            axum::routing::put(crate::speech::library::handler::save_description),
        )
}
