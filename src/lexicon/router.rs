use axum::{Router, routing::get};

use crate::{lexicon::handler, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/detections", axum::routing::post(handler::detect))
        .route(
            "/surface-match-snapshots/{snapshot_id}",
            get(handler::surface_match_snapshot_page),
        )
        .route(
            "/dialect-variant-suggestions",
            axum::routing::post(handler::suggest_dialect_variants),
        )
        .route("/entries", get(handler::list).post(handler::create))
        .route(
            "/entries/archive-batch",
            axum::routing::post(handler::archive_batch),
        )
        .route(
            "/entries/restore-batch",
            axum::routing::post(handler::restore_batch),
        )
        .route("/entries/stats", get(handler::stats))
        .route("/entries/related-search", get(handler::related_search))
        .route(
            "/entries/{id}",
            get(handler::get).delete(handler::delete_draft),
        )
        .route(
            "/entries/{id}/archive",
            axum::routing::post(handler::archive),
        )
        .route(
            "/entries/{id}/restore",
            axum::routing::post(handler::restore),
        )
        .route(
            "/entries/{id}/publications",
            axum::routing::post(handler::publish),
        )
        .route(
            "/entries/{id}/steps/forms/impact",
            axum::routing::post(handler::preview_forms_impact),
        )
        .route(
            "/entries/{id}/steps/forms",
            axum::routing::put(handler::save_forms),
        )
        .route(
            "/entries/{id}/steps/meanings",
            axum::routing::put(handler::save_meanings),
        )
        .route(
            "/entries/{id}/validate",
            axum::routing::post(handler::validate),
        )
}
