use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header},
    response::IntoResponse,
};

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
    params(("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型分组"), ("X-TSZ-Dubbing-Groups"=Option<String>,Header,description="v3 返回显式英美配对或待整理历史项及 order_revision；v2 返回单侧列表；缺省保持旧表示")),
    responses((status = 200, body = DubbingGroupsReadResponse), (status = 401), (status = 403))
)]
pub async fn groups(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
) -> Result<axum::response::Response, AppError> {
    require_active_admin(&state, &auth).await?;
    if paired_supported(&headers) {
        let result = VoiceLibraryRepository::new(state.pool)
            .groups_v3(auth.subject)
            .await?;
        return Ok((
            [(header::VARY, "x-tsz-dubbing-groups, x-tsz-voice-models")],
            Json(result),
        )
            .into_response());
    }
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
    Ok((
        [(header::VARY, "x-tsz-dubbing-groups, x-tsz-voice-models")],
        Json(value),
    )
        .into_response())
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
    put, path = "/api/v1/admin/speech/groups/order", tag = "admin-speech", security(("bearer_auth"=[])),
    request_body = SaveDubbingGroupOrderRequest,
    responses((status=200,body=DubbingGroupsV3Response),(status=400),(status=401),(status=403),(status=409),(status=422))
)]
pub async fn save_order(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SaveDubbingGroupOrderRequest>,
) -> Result<Json<DubbingGroupsV3Response>, AppError> {
    require_active_admin(&state, &auth).await?;
    let (_, run) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    Ok(Json(
        VoiceLibraryRepository::new(state.pool)
            .with_test_run(run)
            .save_order(auth.subject, auth.security_version, request)
            .await?,
    ))
}

#[utoipa::path(
    post, path = "/api/v1/admin/speech/groups", tag = "admin-speech", security(("bearer_auth"=[])),
    request_body = DubbingGroupWriteRequest,
    params(("X-TSZ-Dubbing-Groups"=Option<String>,Header,description="v3 用于配对推荐")),
    responses((status=200,body=DubbingGroupsReadResponse),(status=400),(status=403),(status=409),(status=422))
)]
pub async fn create_group(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    ApiJson(request): ApiJson<DubbingGroupWriteRequest>,
) -> Result<Json<DubbingGroupsReadResponse>, AppError> {
    write_group(state, auth, headers, request, true).await
}

#[utoipa::path(
    put, path = "/api/v1/admin/speech/groups/{id}", tag = "admin-speech", security(("bearer_auth"=[])),
    params(("id"=uuid::Uuid,Path),("X-TSZ-Dubbing-Groups"=Option<String>,Header,description="v3 用于配对推荐")), request_body = DubbingGroupWriteRequest,
    responses((status=200,body=DubbingGroupsReadResponse),(status=400),(status=403),(status=409),(status=422))
)]
pub async fn save_group(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<uuid::Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<DubbingGroupWriteRequest>,
) -> Result<Json<DubbingGroupsReadResponse>, AppError> {
    let group_id = match &request {
        DubbingGroupWriteRequest::Legacy(input) => input.group.id,
        DubbingGroupWriteRequest::Paired(input) => input.group.id,
    };
    if id != group_id {
        return Err(crate::error::AppError::bad_request(
            crate::error::ErrorCode::InvalidRequestBody,
            "分组身份不匹配",
        ));
    }
    write_group(state, auth, headers, request, false).await
}

fn paired_supported(headers: &HeaderMap) -> bool {
    headers
        .get("x-tsz-dubbing-groups")
        .is_some_and(|value| value == "v3")
}

async fn write_group(
    state: AppState,
    auth: AdminAuth,
    headers: HeaderMap,
    request: DubbingGroupWriteRequest,
    create: bool,
) -> Result<Json<DubbingGroupsReadResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    let (_, run) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    let repo = VoiceLibraryRepository::new(state.pool).with_test_run(run);
    let result = match (paired_supported(&headers), request) {
        (true, DubbingGroupWriteRequest::Paired(input)) => DubbingGroupsReadResponse::Paired(
            repo.save_paired_group(auth.subject, auth.security_version, input, create)
                .await?,
        ),
        (false, DubbingGroupWriteRequest::Legacy(input)) => DubbingGroupsReadResponse::Legacy(
            repo.save_group(auth.subject, auth.security_version, input, create)
                .await?,
        ),
        _ => {
            return Err(AppError::bad_request(
                crate::error::ErrorCode::InvalidRequestBody,
                "配对请求必须使用 X-TSZ-Dubbing-Groups: v3",
            ));
        }
    };
    Ok(Json(result))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/speech/groups/{id}", tag = "admin-speech", security(("bearer_auth"=[])),
    params(("id"=uuid::Uuid,Path),("X-TSZ-Dubbing-Groups"=Option<String>,Header,description="v3 删除配对组")), request_body = DeleteDubbingGroupRequest,
    responses((status=200,body=DubbingGroupsReadResponse),(status=400),(status=403),(status=404),(status=409),(status=422))
)]
pub async fn delete_group(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<uuid::Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<DeleteDubbingGroupRequest>,
) -> Result<Json<DubbingGroupsReadResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    let (_, run) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    let repo = VoiceLibraryRepository::new(state.pool).with_test_run(run);
    Ok(Json(if paired_supported(&headers) {
        DubbingGroupsReadResponse::Paired(
            repo.delete_paired_group(auth.subject, auth.security_version, id, request)
                .await?,
        )
    } else {
        DubbingGroupsReadResponse::Legacy(
            repo.delete_group(auth.subject, auth.security_version, id, request)
                .await?,
        )
    }))
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
