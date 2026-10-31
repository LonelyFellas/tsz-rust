use axum::{Router, routing::get};

use crate::state::AppState;

use super::handler;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/voices", get(handler::list_voices))
        .route("/previews", axum::routing::post(handler::create_preview))
        .route("/ssml", axum::routing::post(handler::render_ssml))
        .route(
            "/previews/ssml",
            axum::routing::post(handler::manual_preview),
        )
        .route("/previews/{hash}/content", get(handler::preview_content))
        .route(
            "/resolve",
            axum::routing::post(crate::speech::dubbing::resolve),
        )
        .route(
            "/audio",
            axum::routing::post(crate::speech::generated::generate),
        )
        .route(
            "/audio-update-settings",
            get(crate::speech::audio_updates::settings)
                .put(crate::speech::audio_updates::save_settings),
        )
        .route(
            "/groups/{id}/audio-updates",
            get(crate::speech::audio_updates::get_updates),
        )
        .route(
            "/audio-updates",
            get(crate::speech::audio_updates::all_updates),
        )
        .route(
            "/audio-updates/{id}/cancel",
            axum::routing::post(crate::speech::audio_updates::cancel),
        )
        .route(
            "/audio-updates/{id}/retry",
            axum::routing::post(crate::speech::audio_updates::retry),
        )
        .route("/library", get(crate::speech::library::handler::list))
        .route(
            "/groups",
            get(crate::speech::library::handler::groups)
                .put(crate::speech::library::handler::save_groups)
                .post(crate::speech::library::handler::create_group),
        )
        .route(
            "/groups/order",
            axum::routing::put(crate::speech::library::handler::save_order),
        )
        .route(
            "/groups/{id}",
            axum::routing::put(crate::speech::library::handler::save_group)
                .delete(crate::speech::library::handler::delete_group),
        )
        .route(
            "/library/{alias}/personal",
            axum::routing::put(crate::speech::library::handler::save_settings),
        )
        .route(
            "/library/{alias}/accent",
            axum::routing::put(crate::speech::library::handler::save_accent),
        )
        .route(
            "/library/{alias}/description/restore",
            axum::routing::post(crate::speech::library::handler::restore_description),
        )
        .route(
            "/library/{alias}/description",
            axum::routing::put(crate::speech::library::handler::save_description),
        )
}
