use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

use crate::{
    admin::{
        AdminAuth,
        authorization::{require_active_admin, require_super_admin},
    },
    api::{ApiJson, ApiPath, ApiQuery, PaginatedResponse},
    catalog::{
        model::{
            CatalogResponse, CreatePartRequest, CreateSubPartRequest, DeleteRevisionQuery,
            PartListQuery, PartOfSpeechConfig, PartPath, SubPartListResponse,
            SubPartOfSpeechConfig, SubPartPath, UpdatePartRequest, UpdateSubPartRequest,
        },
        repository::CatalogRepository,
        service::{CatalogService, CatalogServiceError},
    },
    error::{AppError, ErrorCode, ProblemMeta},
    state::AppState,
};

fn service(state: &AppState) -> CatalogService {
    CatalogService::new(CatalogRepository::new(state.pool.clone()))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/settings/parts-of-speech/catalog",
    tag = "admin-catalog",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "完整词性目录", body = CatalogResponse),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 500, description = "数据库查询失败")
    )
)]
pub async fn catalog(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state).catalog().await.map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/settings/parts-of-speech",
    tag = "admin-catalog",
    security(("bearer_auth" = [])),
    params(PartListQuery),
    responses(
        (status = 200, description = "基本词性管理列表", body = PaginatedResponse<PartOfSpeechConfig>),
        (status = 400, description = "查询参数非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "需要超级管理员"),
        (status = 500, description = "数据库查询失败")
    )
)]
pub async fn list_parts(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<PartListQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_super_admin(&state, &auth).await?;
    let response = service(&state).list_parts(query).await.map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/settings/parts-of-speech",
    tag = "admin-catalog",
    security(("bearer_auth" = [])),
    request_body = CreatePartRequest,
    responses(
        (status = 201, description = "基本词性创建成功", body = PartOfSpeechConfig),
        (status = 400, description = "字段值非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "需要超级管理员"),
        (status = 409, description = "编码、名称或缩写冲突"),
        (status = 422, description = "请求结构非法"),
        (status = 500, description = "数据库写入失败")
    )
)]
pub async fn create_part(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(request): ApiJson<CreatePartRequest>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_super_admin(&state, &auth).await?;
    let response = service(&state)
        .create_part(admin.id, request)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/settings/parts-of-speech/{id}",
    tag = "admin-catalog",
    security(("bearer_auth" = [])),
    params(PartPath),
    request_body = UpdatePartRequest,
    responses(
        (status = 200, description = "基本词性更新成功", body = PartOfSpeechConfig),
        (status = 400, description = "路径或字段值非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "需要超级管理员"),
        (status = 404, description = "基本词性不存在"),
        (status = 409, description = "唯一值或 revision 冲突"),
        (status = 422, description = "请求结构非法"),
        (status = 500, description = "数据库写入失败")
    )
)]
pub async fn update_part(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<PartPath>,
    ApiJson(request): ApiJson<UpdatePartRequest>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_super_admin(&state, &auth).await?;
    let response = service(&state)
        .update_part(admin.id, path.id, request)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/settings/parts-of-speech/{id}",
    tag = "admin-catalog",
    security(("bearer_auth" = [])),
    params(PartPath, DeleteRevisionQuery),
    responses(
        (status = 204, description = "基本词性删除成功"),
        (status = 400, description = "路径或 base_revision 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "需要超级管理员"),
        (status = 404, description = "基本词性不存在"),
        (status = 409, description = "revision 冲突或配置仍被引用"),
        (status = 500, description = "数据库写入失败")
    )
)]
pub async fn delete_part(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<PartPath>,
    ApiQuery(query): ApiQuery<DeleteRevisionQuery>,
) -> Result<StatusCode, AppError> {
    require_super_admin(&state, &auth).await?;
    if let Some(error) = invalid_delete_revision(query.base_revision) {
        return Err(error);
    }
    service(&state)
        .delete_part(path.id, query.base_revision)
        .await
        .map_err(map_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts",
    tag = "admin-catalog",
    security(("bearer_auth" = [])),
    params(PartPath),
    responses(
        (status = 200, description = "细分词性列表", body = SubPartListResponse),
        (status = 400, description = "路径参数非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "需要超级管理员"),
        (status = 404, description = "基本词性不存在"),
        (status = 500, description = "数据库查询失败")
    )
)]
pub async fn list_sub_parts(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<PartPath>,
) -> Result<impl IntoResponse, AppError> {
    require_super_admin(&state, &auth).await?;
    let response = service(&state)
        .list_sub_parts(path.id)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts",
    tag = "admin-catalog",
    security(("bearer_auth" = [])),
    params(PartPath),
    request_body = CreateSubPartRequest,
    responses(
        (status = 201, description = "细分词性创建成功", body = SubPartOfSpeechConfig),
        (status = 400, description = "路径或字段值非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "需要超级管理员"),
        (status = 404, description = "基本词性不存在"),
        (status = 409, description = "编码或同父级名称冲突"),
        (status = 422, description = "请求结构非法"),
        (status = 500, description = "数据库写入失败")
    )
)]
pub async fn create_sub_part(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<PartPath>,
    ApiJson(request): ApiJson<CreateSubPartRequest>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_super_admin(&state, &auth).await?;
    let response = service(&state)
        .create_sub_part(admin.id, path.id, request)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}",
    tag = "admin-catalog",
    security(("bearer_auth" = [])),
    params(SubPartPath),
    request_body = UpdateSubPartRequest,
    responses(
        (status = 200, description = "细分词性更新成功", body = SubPartOfSpeechConfig),
        (status = 400, description = "路径或字段值非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "需要超级管理员"),
        (status = 404, description = "细分词性不存在或父级不匹配"),
        (status = 409, description = "唯一值或 revision 冲突"),
        (status = 422, description = "请求结构非法"),
        (status = 500, description = "数据库写入失败")
    )
)]
pub async fn update_sub_part(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<SubPartPath>,
    ApiJson(request): ApiJson<UpdateSubPartRequest>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_super_admin(&state, &auth).await?;
    let response = service(&state)
        .update_sub_part(admin.id, path.id, path.sub_id, request)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}",
    tag = "admin-catalog",
    security(("bearer_auth" = [])),
    params(SubPartPath, DeleteRevisionQuery),
    responses(
        (status = 204, description = "细分词性删除成功"),
        (status = 400, description = "路径或 base_revision 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "需要超级管理员"),
        (status = 404, description = "细分词性不存在或父级不匹配"),
        (status = 409, description = "revision 冲突或配置仍被引用"),
        (status = 500, description = "数据库写入失败")
    )
)]
pub async fn delete_sub_part(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<SubPartPath>,
    ApiQuery(query): ApiQuery<DeleteRevisionQuery>,
) -> Result<StatusCode, AppError> {
    require_super_admin(&state, &auth).await?;
    if let Some(error) = invalid_delete_revision(query.base_revision) {
        return Err(error);
    }
    service(&state)
        .delete_sub_part(path.id, path.sub_id, query.base_revision)
        .await
        .map_err(map_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn invalid_delete_revision(value: i64) -> Option<AppError> {
    if value < 1 {
        Some(AppError::validation(
            ErrorCode::InvalidQuery,
            "base_revision",
            "base_revision must be at least 1",
        ))
    } else {
        None
    }
}

fn map_error(error: CatalogServiceError) -> AppError {
    match error {
        CatalogServiceError::InvalidPart { field, message } => {
            AppError::validation(ErrorCode::InvalidPartOfSpeech, field, message)
        }
        CatalogServiceError::InvalidQuery(message) => {
            AppError::bad_request(ErrorCode::InvalidQuery, message)
        }
        CatalogServiceError::PartNotFound => AppError::not_found_with_code(
            ErrorCode::PartOfSpeechNotFound,
            "part of speech not found",
        ),
        CatalogServiceError::SubPartNotFound => AppError::not_found_with_code(
            ErrorCode::SubPartOfSpeechNotFound,
            "sub part of speech not found",
        ),
        CatalogServiceError::RevisionConflict {
            current_revision,
            part_of_speech_id,
            code,
        } => AppError::conflict(
            ErrorCode::RevisionConflict,
            Some("base_revision"),
            "configuration changed",
        )
        .with_meta(ProblemMeta {
            current_revision: Some(current_revision),
            part_of_speech_id: Some(part_of_speech_id),
            code: Some(code),
            ..ProblemMeta::default()
        }),
        CatalogServiceError::PartConflict(field) => AppError::conflict(
            ErrorCode::PartOfSpeechConflict,
            Some(field),
            "part of speech already exists",
        ),
        CatalogServiceError::SubPartConflict(field) => AppError::conflict(
            ErrorCode::SubPartOfSpeechConflict,
            Some(field),
            "sub part of speech already exists",
        ),
        CatalogServiceError::PartInUse { usage_count } => {
            let error = AppError::conflict(
                ErrorCode::PartOfSpeechInUse,
                None,
                "part of speech is in use",
            );
            with_usage(error, usage_count)
        }
        CatalogServiceError::SubPartInUse { usage_count } => {
            let error = AppError::conflict(
                ErrorCode::SubPartOfSpeechInUse,
                None,
                "sub part of speech is in use",
            );
            with_usage(error, usage_count)
        }
        other => AppError::internal(other),
    }
}

fn with_usage(error: AppError, usage_count: Option<i64>) -> AppError {
    match usage_count {
        Some(usage_count) => error.with_meta(ProblemMeta {
            usage_count: Some(usage_count),
            ..ProblemMeta::default()
        }),
        None => error,
    }
}
