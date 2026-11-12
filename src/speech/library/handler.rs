use axum::{Json, extract::State, http::HeaderMap, response::IntoResponse};

use crate::{
    admin::{AdminAuth, authorization::require_active_admin},
    api::{ApiJson, ApiPath},
    error::AppError,
    state::AppState,
};

use super::{VoiceLibraryRepository, dto::*};

const GROUPS_HEADER: &str = "x-tsz-dubbing-groups";

fn require_independent_groups(headers: &HeaderMap) -> Result<(), AppError> {
    if headers
        .get(GROUPS_HEADER)
        .is_some_and(|value| value == "v4")
    {
        return Ok(());
    }
    Err(AppError::conflict(
        crate::error::ErrorCode::UnsupportedSchemaVersion,
        Some("groups"),
        "配音分组需要 X-TSZ-Dubbing-Groups: v4，请刷新新版客户端后重试",
    ))
}

/// Gate before JSON decoding so paired legacy requests get an explicit upgrade error.
pub struct IndependentDubbingGroups;

impl axum::extract::FromRequestParts<AppState> for IndependentDubbingGroups {
    type Rejection = AppError;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = <AdminAuth as axum::extract::FromRequestParts<AppState>>::from_request_parts(
            parts, state,
        )
        .await?;
        require_active_admin(state, &auth).await?;
        require_independent_groups(&parts.headers)?;
        Ok(Self)
    }
}

#[utoipa::path(get,path="/api/v1/admin/speech/groups",tag="admin-speech",security(("bearer_auth"=[])),
    params(("X-TSZ-Dubbing-Groups"=Option<String>,Header,description="v4 返回独立单侧列表；v3 仅返回 legacy 单侧只读表示，不能写入")),
    responses((status=200,body=DubbingGroupsReadResponse),(status=401),(status=403),(status=409)))]
pub async fn groups(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
) -> Result<axum::response::Response, AppError> {
    require_active_admin(&state, &auth).await?;
    let legacy = headers
        .get(GROUPS_HEADER)
        .is_some_and(|value| value == "v3");
    let result = VoiceLibraryRepository::new(state.pool)
        .groups(auth.subject)
        .await?;
    let response = if legacy {
        DubbingGroupsReadResponse::Legacy(LegacyDubbingGroupsResponse {
            revision: result.revision,
            order_revision: result.order_revision,
            groups: result
                .groups
                .into_iter()
                .map(|g| LegacyDubbingGroup {
                    id: g.id,
                    name: g.name,
                    areas: g.areas,
                    revision: g.revision,
                    is_global: g.is_global,
                    recommendations: LegacySideRecommendations::Legacy { uk: g.uk, us: g.us },
                })
                .collect(),
        })
    } else if headers
        .get(GROUPS_HEADER)
        .is_some_and(|value| value == "v4")
    {
        DubbingGroupsReadResponse::Independent(result)
    } else {
        let v2 = headers
            .get(GROUPS_HEADER)
            .is_some_and(|value| value == "v2");
        let side = |items: Vec<DubbingRecommendation>| {
            items.into_iter()
            .filter(|item| v2 || item.selected)
            .map(|item| if v2 { serde_json::json!(item) } else {
                serde_json::json!({"id":item.id,"name":item.name,"voice_alias":item.voice_alias,"parameters":item.parameters})
            }).collect()
        };
        DubbingGroupsReadResponse::Unversioned(UnversionedDubbingGroupsResponse {
            revision: result.revision,
            groups: result
                .groups
                .into_iter()
                .filter(|group| v2 || !group.is_global)
                .map(|g| UnversionedDubbingGroup {
                    id: g.id,
                    name: g.name,
                    areas: g.areas,
                    uk: side(g.uk),
                    us: side(g.us),
                    revision: v2.then_some(g.revision),
                    is_global: v2.then_some(g.is_global),
                })
                .collect(),
        })
    };
    Ok(([("vary", GROUPS_HEADER)], Json(response)).into_response())
}

#[utoipa::path(
    put, path = "/api/v1/admin/speech/groups", tag = "admin-speech",
    security(("bearer_auth" = [])), request_body = SaveDubbingGroupsRequest,
    params(("X-TSZ-Dubbing-Groups" = String, Header, description = "v4 独立单侧分组写入；旧版本只读并拒绝写入", example = "v4"), ("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型分组")),
    responses((status = 200, body = DubbingGroupsResponse), (status = 400), (status = 401),
        (status = 403), (status = 404), (status = 409), (status = 422))
)]
pub async fn save_groups(
    State(state): State<AppState>,
    auth: AdminAuth,
    _protocol: IndependentDubbingGroups,
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
    params(("X-TSZ-Dubbing-Groups" = String, Header, description = "v4 独立单侧分组写入；旧版本只读并拒绝写入", example = "v4")),
    request_body = SaveDubbingGroupOrderRequest,
    responses((status=200,body=DubbingGroupsResponse),(status=400),(status=401),(status=403),(status=409),(status=422))
)]
pub async fn save_order(
    State(state): State<AppState>,
    auth: AdminAuth,
    _protocol: IndependentDubbingGroups,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SaveDubbingGroupOrderRequest>,
) -> Result<Json<DubbingGroupsResponse>, AppError> {
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
    params(("X-TSZ-Dubbing-Groups" = String, Header, description = "v4 独立单侧分组写入；旧版本只读并拒绝写入", example = "v4")),
    request_body = SaveDubbingGroupRequest,
    responses((status=200,body=DubbingGroupsResponse),(status=400),(status=403),(status=409),(status=422))
)]
pub async fn create_group(
    State(state): State<AppState>,
    auth: AdminAuth,
    _protocol: IndependentDubbingGroups,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SaveDubbingGroupRequest>,
) -> Result<Json<DubbingGroupsResponse>, AppError> {
    write_group(state, auth, headers, request, true).await
}

#[utoipa::path(
    put, path = "/api/v1/admin/speech/groups/{id}", tag = "admin-speech", security(("bearer_auth"=[])),
    params(("X-TSZ-Dubbing-Groups" = String, Header, description = "v4 独立单侧分组写入；旧版本只读并拒绝写入", example = "v4"), ("id"=uuid::Uuid,Path)), request_body = SaveDubbingGroupRequest,
    responses((status=200,body=DubbingGroupsResponse),(status=400),(status=403),(status=409),(status=422))
)]
pub async fn save_group(
    State(state): State<AppState>,
    auth: AdminAuth,
    _protocol: IndependentDubbingGroups,
    ApiPath(id): ApiPath<uuid::Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SaveDubbingGroupRequest>,
) -> Result<Json<DubbingGroupsResponse>, AppError> {
    let group_id = request.group.id;
    if id != group_id {
        return Err(crate::error::AppError::bad_request(
            crate::error::ErrorCode::InvalidRequestBody,
            "分组身份不匹配",
        ));
    }
    write_group(state, auth, headers, request, false).await
}

async fn write_group(
    state: AppState,
    auth: AdminAuth,
    headers: HeaderMap,
    request: SaveDubbingGroupRequest,
    create: bool,
) -> Result<Json<DubbingGroupsResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    let (_, run) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    let repo = VoiceLibraryRepository::new(state.pool).with_test_run(run);
    let result = repo
        .save_group(auth.subject, auth.security_version, request, create)
        .await?;
    Ok(Json(result))
}

#[utoipa::path(
    delete, path = "/api/v1/admin/speech/groups/{id}", tag = "admin-speech", security(("bearer_auth"=[])),
    params(("X-TSZ-Dubbing-Groups" = String, Header, description = "v4 独立单侧分组写入；旧版本只读并拒绝写入", example = "v4"), ("id"=uuid::Uuid,Path)), request_body = DeleteDubbingGroupRequest,
    responses((status=200,body=DubbingGroupsResponse),(status=400),(status=403),(status=404),(status=409),(status=422))
)]
pub async fn delete_group(
    State(state): State<AppState>,
    auth: AdminAuth,
    _protocol: IndependentDubbingGroups,
    ApiPath(id): ApiPath<uuid::Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<DeleteDubbingGroupRequest>,
) -> Result<Json<DubbingGroupsResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    let (_, run) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    let repo = VoiceLibraryRepository::new(state.pool).with_test_run(run);
    Ok(Json(
        repo.delete_group(auth.subject, auth.security_version, id, request)
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
