use axum::{Router, extract::DefaultBodyLimit, routing::get};

use crate::{
    lexicon::{handler, validation::MAX_STEP_CONTENT_BODY_BYTES},
    state::AppState,
};

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
            "/entries/delete-batch",
            axum::routing::post(handler::delete_batch),
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
            get(handler::list_publications).post(handler::publish),
        )
        .route(
            "/entries/{id}/publications/{publication_id}",
            get(handler::get_publication),
        )
        .route(
            "/entries/{id}/publications/{publication_id}/activate",
            axum::routing::post(handler::activate_publication),
        )
        // 只有这三条路由承载整步草稿内容，需要高于 axum 默认 2 MiB 的请求体上限。
        // 其余接口的请求体都被自身契约框住（批量最多 100 条、其余是定长字段），
        // 继续吃默认值即可，不跟着放宽。
        .route(
            "/entries/{id}/steps/forms/impact",
            axum::routing::post(handler::preview_forms_impact)
                .layer(DefaultBodyLimit::max(MAX_STEP_CONTENT_BODY_BYTES)),
        )
        .route(
            "/entries/{id}/steps/forms",
            axum::routing::put(handler::save_forms)
                .layer(DefaultBodyLimit::max(MAX_STEP_CONTENT_BODY_BYTES)),
        )
        .route(
            "/entries/{id}/steps/meanings",
            axum::routing::put(handler::save_meanings)
                .layer(DefaultBodyLimit::max(MAX_STEP_CONTENT_BODY_BYTES)),
        )
        .route(
            "/entries/{id}/sentences/{sentence_id}/associations",
            axum::routing::put(handler::replace_sentence_associations),
        )
        .route(
            "/entries/sentence-targets/resolve",
            axum::routing::post(handler::resolve_sentence_targets),
        )
        .route(
            "/entries/component-targets/search",
            axum::routing::post(handler::search_component_targets),
        )
        .route(
            "/entries/{id}/pending-sentence-associations",
            get(handler::list_pending_sentence_associations),
        )
        .route(
            "/pending-sentence-associations/{association_id}/claim",
            axum::routing::post(handler::claim_pending_sentence_association),
        )
        .route(
            "/entries/{id}/content-completion-jobs",
            axum::routing::post(
                crate::lexicon::content_completion::handler::create_content_completion_job,
            ),
        )
        .route(
            "/entries/{id}/content-completion-jobs/{job_id}",
            get(crate::lexicon::content_completion::handler::get_content_completion_job),
        )
        .route(
            "/entries/{id}/content-completion-jobs/{job_id}/retries",
            axum::routing::post(
                crate::lexicon::content_completion::handler::retry_content_completion_job,
            ),
        )
        .route(
            "/entries/{id}/validate",
            axum::routing::post(handler::validate),
        )
}
