use axum::{
    Router,
    routing::{get, patch},
};

use crate::{catalog::handler, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(handler::list_parts).post(handler::create_part))
        .route("/catalog", get(handler::catalog))
        .route(
            "/{id}",
            patch(handler::update_part).delete(handler::delete_part),
        )
        .route(
            "/{id}/sub-parts",
            get(handler::list_sub_parts).post(handler::create_sub_part),
        )
        .route(
            "/{id}/sub-parts/{sub_id}",
            patch(handler::update_sub_part).delete(handler::delete_sub_part),
        )
}
