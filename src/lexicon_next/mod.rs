//! Independent language-aware drafts; no legacy identities.
pub mod language;
pub mod model;
mod repository;

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
use model::*;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/capabilities", get(capabilities))
        .route("/entries", get(list).post(create))
        .route("/entries/{id}", get(read).put(save))
        .route("/entries/{id}/view", get(view))
        .layer(DefaultBodyLimit::max(512 * 1024))
}

#[utoipa::path(get, path="/api/v1/admin/lexicon-next/capabilities", tag="admin-lexicon-next", security(("bearer_auth"=[])), responses((status=200, body=NextCapabilities), (status=401), (status=403)))]
pub async fn capabilities(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Json<NextCapabilities>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(language::capabilities()))
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
    ApiJson(input): ApiJson<CreateNextEntry>,
) -> Result<(StatusCode, Json<NextEntry>), AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.create")?;
    Ok((
        StatusCode::CREATED,
        Json(repository::create(&state.pool, &auth, input).await?),
    ))
}
#[utoipa::path(get, path="/api/v1/admin/lexicon-next/entries/{id}", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("id"=Uuid, Path)), responses((status=200, body=NextEntry), (status=401), (status=403), (status=404)))]
pub async fn read(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<NextEntry>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(repository::get(&state.pool, id).await?))
}
#[utoipa::path(put, path="/api/v1/admin/lexicon-next/entries/{id}", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("id"=Uuid, Path)), request_body=SaveNextDraft, responses((status=200, body=NextEntry), (status=401), (status=403), (status=404), (status=409, description="revision_conflict or stable_node_id_changed"), (status=422)))]
pub async fn save(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<SaveNextDraft>,
) -> Result<Json<NextEntry>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.edit")?;
    Ok(Json(repository::save(&state.pool, &auth, id, input).await?))
}
#[utoipa::path(get, path="/api/v1/admin/lexicon-next/entries/{id}/view", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("id"=Uuid, Path), NextViewQuery), responses((status=200, body=NextEntryView), (status=401), (status=403), (status=404), (status=422, description="capability_unavailable")))]
pub async fn view(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<NextViewQuery>,
) -> Result<Json<NextEntryView>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    Ok(Json(language::view(
        &repository::get(&state.pool, id).await?,
        query,
    )?))
}

#[cfg(test)]
mod tests;
