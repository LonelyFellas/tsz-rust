use axum::{Json, extract::State, http::HeaderMap};

use crate::{
    admin::{AdminAuth, authorization::require_active_admin},
    api::{ApiJson, ApiPath},
    error::AppError,
    state::AppState,
};

use super::{VoiceLibraryRepository, dto::*};

#[utoipa::path(
    get, path = "/api/v1/admin/speech/groups", tag = "admin-speech",
    security(("bearer_auth" = [])),
    params(("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型分组；旧客户端保留条目但不能覆盖含模型分组")),
    responses((status = 200, body = DubbingGroupsResponse), (status = 401), (status = 403))
)]
pub async fn groups(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Json<DubbingGroupsResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .groups(auth.subject)
            .await?,
    ))
}

#[utoipa::path(
    put, path = "/api/v1/admin/speech/groups", tag = "admin-speech",
    security(("bearer_auth" = [])), request_body = SaveDubbingGroupsRequest,
    params(("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型分组")),
    responses((status = 200, body = DubbingGroupsResponse), (status = 400), (status = 401),
        (status = 403), (status = 404), (status = 409), (status = 422))
)]
pub async fn save_groups(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SaveDubbingGroupsRequest>,
) -> Result<Json<DubbingGroupsResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .save_groups_with_models(
                auth.subject,
                auth.security_version,
                request,
                crate::lexicon::voice_versions::models_supported(&headers),
            )
            .await?,
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/speech/library", tag = "admin-speech",
    security(("bearer_auth" = [])),
    params(("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型分组；旧客户端保留条目但不能覆盖含模型分组")),
    responses((status = 200, body = VoiceLibraryResponse), (status = 401), (status = 403))
)]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Json<VoiceLibraryResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .list(auth.subject)
            .await?,
    ))
}

#[utoipa::path(
    put, path = "/api/v1/admin/speech/library/{alias}/personal", tag = "admin-speech",
    security(("bearer_auth" = [])), params(("alias" = String, Path),("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型资料及参数")),
    request_body = SaveVoiceSettingsRequest,
    responses((status = 200, body = VoiceLibraryItem), (status = 400), (status = 401),
        (status = 403), (status = 404), (status = 409), (status = 422))
)]
pub async fn save_settings(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(alias): ApiPath<String>,
    ApiJson(request): ApiJson<SaveVoiceSettingsRequest>,
) -> Result<Json<VoiceLibraryItem>, AppError> {
    require_active_admin(&state, &auth).await?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .save_settings(auth.subject, &alias, auth.security_version, request)
            .await?,
    ))
}

#[utoipa::path(
    put, path = "/api/v1/admin/speech/library/{alias}/description", tag = "admin-speech",
    security(("bearer_auth" = [])), params(("alias" = String, Path),("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型资料及参数")),
    request_body = SaveVoiceDescriptionRequest,
    responses((status = 200, body = VoiceLibraryItem), (status = 400), (status = 401),
        (status = 403), (status = 404), (status = 409), (status = 422))
)]
pub async fn save_description(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(alias): ApiPath<String>,
    ApiJson(request): ApiJson<SaveVoiceDescriptionRequest>,
) -> Result<Json<VoiceLibraryItem>, AppError> {
    require_active_admin(&state, &auth).await?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .save_description(auth.subject, &alias, auth.security_version, request)
            .await?,
    ))
}
