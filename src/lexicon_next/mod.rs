//! Independent language-aware drafts; no legacy identities.
pub mod detection;
pub mod entry_matches;
mod forms;
mod grammar;
pub mod language;
pub mod list_model;
pub mod model;
mod repository;
mod workspace;

use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiJson, ApiPath, ApiQuery},
    error::AppError,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::get,
};
use list_model::*;
use model::*;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/workspace/entries", get(workspace_list))
        .route("/workspace/entries/{id}", get(workspace_read))
        .route(
            "/workspace/entries/{id}/annotation",
            axum::routing::patch(workspace_annotation),
        )
        .route("/workspace/stats", get(workspace_stats))
        .route("/workspace/creators", get(workspace_creators))
        .route(
            "/workspace/commands",
            axum::routing::post(workspace_command),
        )
        .route("/capabilities", get(capabilities))
        .route("/detections", axum::routing::post(detect))
        .route("/entry-matches", axum::routing::post(match_entries))
        .route("/entries", get(list).post(create))
        .route("/entries/{id}", get(read).put(save))
        .route("/entries/{id}/view", get(view))
        .layer(DefaultBodyLimit::max(
            crate::lexicon::validation::MAX_STEP_CONTENT_BODY_BYTES + 512 * 1024,
        ))
}

#[utoipa::path(post, path="/api/v1/admin/lexicon-next/entry-matches", tag="admin-lexicon-next", security(("bearer_auth"=[])), request_body=entry_matches::NextEntryMatchInput, responses((status=200, body=entry_matches::NextEntryMatches), (status=401), (status=403), (status=422)))]
pub async fn match_entries(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<entry_matches::NextEntryMatchInput>,
) -> Result<Json<entry_matches::NextEntryMatches>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.create")?;
    Ok(Json(entry_matches::find(&state.pool, input).await?))
}

#[utoipa::path(post, path="/api/v1/admin/lexicon-next/detections", tag="admin-lexicon-next", security(("bearer_auth"=[])), request_body=detection::NextDetectionInput, responses((status=200, body=detection::NextDetection), (status=401), (status=403), (status=422), (status=503)))]
pub async fn detect(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<detection::NextDetectionInput>,
) -> Result<Json<detection::NextDetection>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.create")?;
    Ok(Json(detection::detect(&state.pool, input).await?))
}

#[utoipa::path(get, path="/api/v1/admin/lexicon-next/capabilities", tag="admin-lexicon-next", security(("bearer_auth"=[])), responses((status=200, body=NextCapabilities), (status=401), (status=403)))]
pub async fn capabilities(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: axum::http::HeaderMap,
) -> Result<Json<NextCapabilities>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    let mut capability = language::capabilities();
    if headers
        .get("x-tsz-next-forms")
        .is_some_and(|value| value == "v1")
    {
        capability.form_editor = Some(true);
    }
    if supports_grammar(&headers) {
        capability.grammar_structures = Some(true);
    }
    if supports_sense_groups(&headers) {
        capability.sense_groups = Some(true);
    }
    if supports_medium_ownership(&headers) {
        capability.medium_ownership = Some(true);
    }
    Ok(Json(capability))
}

#[utoipa::path(get, path="/api/v1/admin/lexicon-next/entries", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(NextListQuery), responses((status=200, body=NextEntryList), (status=401), (status=403), (status=422)))]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<NextListQuery>,
) -> Result<Json<NextEntryList>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(repository::list(&state.pool, query).await?))
}
#[utoipa::path(post, path="/api/v1/admin/lexicon-next/entries", tag="admin-lexicon-next", security(("bearer_auth"=[])), request_body=CreateNextEntry, responses((status=201, body=NextEntry), (status=401), (status=403), (status=422)))]
pub async fn create(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<CreateNextEntry>,
) -> Result<(StatusCode, Json<NextEntry>), AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.create")?;
    Ok((
        StatusCode::CREATED,
        Json(entry_for_client(
            repository::create(&state.pool, &auth, input).await?,
            &headers,
        )),
    ))
}
#[utoipa::path(get, path="/api/v1/admin/lexicon-next/entries/{id}", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("id"=Uuid, Path)), responses((status=200, body=NextEntry), (status=401), (status=403), (status=404)))]
pub async fn read(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: axum::http::HeaderMap,
) -> Result<Json<NextEntry>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(entry_for_client(
        repository::get(&state.pool, id).await?,
        &headers,
    )))
}
#[utoipa::path(put, path="/api/v1/admin/lexicon-next/entries/{id}", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("id"=Uuid, Path)), request_body=SaveNextDraft, responses((status=200, body=NextEntry), (status=401), (status=403), (status=404), (status=409, description="revision_conflict or stable_node_id_changed"), (status=422)))]
pub async fn save(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<SaveNextDraft>,
) -> Result<Json<NextEntry>, AppError> {
    if input.draft.grammar_structures.is_some() && !supports_grammar(&headers) {
        return Err(AppError::unprocessable(
            crate::error::ErrorCode::CapabilityUnavailable,
            "grammar structures require x-tsz-next-grammar: v1",
        ));
    }
    permissions::load(&state, &auth)
        .await?
        .require("words.edit")?;
    Ok(Json(entry_for_client(
        repository::save(&state.pool, &auth, id, input).await?,
        &headers,
    )))
}
#[utoipa::path(get, path="/api/v1/admin/lexicon-next/entries/{id}/view", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("id"=Uuid, Path), NextViewQuery), responses((status=200, body=NextEntryView), (status=401), (status=403), (status=404), (status=422, description="capability_unavailable")))]
pub async fn view(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: axum::http::HeaderMap,
    ApiQuery(query): ApiQuery<NextViewQuery>,
) -> Result<Json<NextEntryView>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    language::require_medium(&query.medium_locale)?;
    let entry = repository::get(&state.pool, id).await?;
    if supports_medium_ownership(&headers)
        && entry.medium_locale.as_deref() != Some(query.medium_locale.as_str())
    {
        return Err(AppError::not_found(
            "entry belongs to another medium locale",
        ));
    }
    let mut view = language::view(&entry, query)?;
    if supports_grammar(&headers) {
        let mut conn = state.pool.acquire().await.map_err(AppError::internal)?;
        view.grammar_issues = entry.draft.grammar_structures.as_ref().map(|_| Vec::new());
        if view.grammar_issues.is_some() {
            view.grammar_issues = Some(grammar::issues(
                &entry.draft,
                &grammar::catalog(&mut conn).await?,
            ));
        }
    } else {
        view.grammar_structures = None;
        for sense in &mut view.senses {
            for definition in sense
                .target_definitions
                .iter_mut()
                .chain(sense.medium_definitions.iter_mut())
            {
                definition.grammar_structure_id = None;
            }
        }
    }
    if !supports_sense_groups(&headers) {
        view.sense_groups = None;
        for sense in &mut view.senses {
            sense.sense_group_id = None;
        }
    }
    Ok(Json(view))
}

fn supports_grammar(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-grammar")
        .is_some_and(|value| value == "v1")
}

fn supports_medium_ownership(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-medium-ownership")
        .is_some_and(|value| value == "v1")
}

fn supports_sense_groups(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-tsz-next-sense-groups")
        .is_some_and(|value| value == "v1")
}

fn entry_for_client(mut entry: NextEntry, headers: &axum::http::HeaderMap) -> NextEntry {
    if !supports_grammar(headers) {
        entry.draft.grammar_structures = None;
        for sense in &mut entry.draft.senses {
            sense.sub_pos = None;
            for definition in &mut sense.definitions {
                definition.grammar_structure_id = None;
            }
        }
    }
    if !supports_medium_ownership(headers) {
        entry.medium_locale = None;
    }
    if !supports_sense_groups(headers) {
        entry.draft.sense_groups = None;
        for sense in &mut entry.draft.senses {
            sense.sense_group_id = None;
        }
    }
    entry
}

#[cfg(test)]
mod tests;

#[utoipa::path(get,path="/api/v1/admin/lexicon-next/workspace/entries",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(NextWorkspaceQuery),responses((status=200,body=NextWorkspaceList),(status=401),(status=403),(status=422)))]
pub async fn workspace_list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<NextWorkspaceQuery>,
) -> Result<Json<NextWorkspaceList>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(workspace::list(&state.pool, query).await?))
}
#[utoipa::path(get,path="/api/v1/admin/lexicon-next/workspace/entries/{id}",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,body=NextWorkspaceEntry),(status=401),(status=403),(status=404)))]
pub async fn workspace_read(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<NextWorkspaceEntry>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(workspace::get(&state.pool, id).await?))
}
#[utoipa::path(get,path="/api/v1/admin/lexicon-next/workspace/stats",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(NextWorkspaceQuery),responses((status=200,body=NextWorkspaceStats),(status=401),(status=403),(status=422)))]
pub async fn workspace_stats(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<NextWorkspaceQuery>,
) -> Result<Json<NextWorkspaceStats>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(workspace::stats(&state.pool, query).await?))
}
#[utoipa::path(get,path="/api/v1/admin/lexicon-next/workspace/creators",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(NextWorkspaceQuery),responses((status=200,body=Vec<NextWorkspaceCreator>),(status=401),(status=403),(status=422)))]
pub async fn workspace_creators(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<NextWorkspaceQuery>,
) -> Result<Json<Vec<NextWorkspaceCreator>>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(workspace::creators(&state.pool, query).await?))
}
#[utoipa::path(patch,path="/api/v1/admin/lexicon-next/workspace/entries/{id}/annotation",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=NextAnnotationInput,responses((status=200,body=NextAnnotationResult),(status=401),(status=403),(status=404),(status=409),(status=422)))]
pub async fn workspace_annotation(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<NextAnnotationInput>,
) -> Result<Json<NextAnnotationResult>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.edit")?;
    Ok(Json(
        workspace::annotation(&state.pool, &auth, id, input).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/admin/lexicon-next/workspace/commands",tag="admin-lexicon-next",security(("bearer_auth"=[])),request_body=NextListCommand,responses((status=200,body=NextListCommandResult),(status=401),(status=403),(status=404),(status=409),(status=422)))]
pub async fn workspace_command(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<NextListCommand>,
) -> Result<Json<NextListCommandResult>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(workspace::command(&state.pool, &auth, input).await?))
}
