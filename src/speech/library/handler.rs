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
    params(("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型分组"), ("X-TSZ-Dubbing-Groups"=Option<String>,Header,description="v2 返回全局常用、单组 revision 与 selected；旧读取只返回已选普通分组")),
    responses((status = 200, body = DubbingGroupsResponse), (status = 401), (status = 403))
)]
pub async fn groups(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    require_active_admin(&state, &auth).await?;
    let result = VoiceLibraryRepository::new(state.pool)
        .groups(auth.subject)
        .await?;
    let mut value = serde_json::to_value(result).map_err(AppError::internal)?;
    if headers
        .get("x-tsz-dubbing-groups")
        .is_none_or(|value| value != "v2")
        && let Some(groups) = value["groups"].as_array_mut()
    {
        groups.retain(|group| group["is_global"] != true);
        for group in groups {
            let fields = group.as_object_mut().expect("group object");
            fields.remove("revision");
            fields.remove("is_global");
            for key in ["uk", "us"] {
                if let Some(items) = fields
                    .get_mut(key)
                    .and_then(serde_json::Value::as_array_mut)
                {
                    items.retain(|item| item["selected"] == true);
                    for item in items {
                        item.as_object_mut()
                            .expect("recommendation object")
                            .remove("selected");
                    }
                }
            }
        }
    }
    Ok(Json(value))
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
    _headers: HeaderMap,
    ApiJson(request): ApiJson<SaveDubbingGroupsRequest>,
) -> Result<Json<DubbingGroupsResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .save_groups(auth.subject, auth.security_version, request)
            .await?,
    ))
}

#[utoipa::path(
    post, path = "/api/v1/admin/speech/groups", tag = "admin-speech", security(("bearer_auth"=[])),
    request_body = SaveDubbingGroupRequest,
    responses((status=200,body=DubbingGroupsResponse),(status=400),(status=403),(status=409),(status=422))
)]
pub async fn create_group(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SaveDubbingGroupRequest>,
) -> Result<Json<DubbingGroupsResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    let (_, run) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .with_test_run(run)
            .save_group(auth.subject, auth.security_version, request, true)
            .await?,
    ))
}

#[utoipa::path(
    put, path = "/api/v1/admin/speech/groups/{id}", tag = "admin-speech", security(("bearer_auth"=[])),
    params(("id"=uuid::Uuid,Path)), request_body = SaveDubbingGroupRequest,
    responses((status=200,body=DubbingGroupsResponse),(status=400),(status=403),(status=409),(status=422))
)]
pub async fn save_group(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<uuid::Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SaveDubbingGroupRequest>,
) -> Result<Json<DubbingGroupsResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    if id != request.group.id {
        return Err(AppError::bad_request(
            crate::error::ErrorCode::InvalidRequestBody,
            "分组身份不匹配",
        ));
    }
    let (_, run) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .with_test_run(run)
            .save_group(auth.subject, auth.security_version, request, false)
            .await?,
    ))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/speech/groups/{id}", tag = "admin-speech", security(("bearer_auth"=[])),
    params(("id"=uuid::Uuid,Path)), request_body = DeleteDubbingGroupRequest,
    responses((status=200,body=DubbingGroupsResponse),(status=400),(status=403),(status=404),(status=409),(status=422))
)]
pub async fn delete_group(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<uuid::Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<DeleteDubbingGroupRequest>,
) -> Result<Json<DubbingGroupsResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    let (_, run) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .with_test_run(run)
            .delete_group(auth.subject, auth.security_version, id, request)
            .await?,
    ))
}

#[utoipa::path(
    get, path = "/api/v1/admin/speech/library", tag = "admin-speech",
    security(("bearer_auth" = [])),
    params(("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型资料")),
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
    put, path = "/api/v1/admin/speech/library/{alias}/accent", tag = "admin-speech",
    security(("bearer_auth" = [])), params(("alias" = String, Path)),
    request_body = SaveVoiceAccentRequest,
    responses((status = 200, body = VoiceLibraryItem), (status = 400), (status = 401),
        (status = 403), (status = 404), (status = 409), (status = 422))
)]
pub async fn save_accent(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(alias): ApiPath<String>,
    ApiJson(request): ApiJson<SaveVoiceAccentRequest>,
) -> Result<Json<VoiceLibraryItem>, AppError> {
    require_active_admin(&state, &auth).await?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .save_accent(auth.subject, &alias, auth.security_version, request)
            .await?,
    ))
}

#[utoipa::path(
    post, path = "/api/v1/admin/speech/library/{alias}/description/restore", tag = "admin-speech",
    security(("bearer_auth" = [])), params(("alias" = String, Path)),
    request_body = RestoreVoiceDescriptionRequest,
    responses((status = 200, body = VoiceLibraryItem), (status = 400), (status = 401),
        (status = 403), (status = 404), (status = 409), (status = 422))
)]
pub async fn restore_description(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(alias): ApiPath<String>,
    ApiJson(request): ApiJson<RestoreVoiceDescriptionRequest>,
) -> Result<Json<VoiceLibraryItem>, AppError> {
    require_active_admin(&state, &auth).await?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .restore_description(auth.subject, &alias, auth.security_version, request)
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
