use axum::{Router, routing::get};

use crate::state::AppState;

use super::handler;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/voices", get(handler::list_voices))
        .route("/previews", axum::routing::post(handler::create_preview))
}
