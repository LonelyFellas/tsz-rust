use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};

use crate::{
    admin::{AdminAuth, authorization::require_active_admin},
    api::{ApiJson, ApiPath},
    error::{AppError, ErrorCode, ProblemMeta},
    lexicon::content_completion::{
        dto::*,
        repository::{ContentCompletionRepository, ContentCompletionRepositoryError},
    },
    request_id::RequestId,
    state::AppState,
};

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/{id}/content-completion-jobs",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(crate::lexicon::dto::EntryPath, ("Idempotency-Key" = uuid::Uuid, Header, description = "生成任务幂等键（UUID）")),
    request_body = CreateContentCompletionJobInput,
    responses(
        (status = 202, description = "生成任务已持久化并等待执行", body = ContentCompletionJobEnvelope),
        (status = 400, description = "Idempotency-Key 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision 或幂等键冲突"),
        (status = 422, description = "归档词条、scope 不完整或无词性"),
        (status = 503, description = "真实生成提供方未配置")
    )
)]
pub async fn create_content_completion_job(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(_request_id): Extension<RequestId>,
    headers: HeaderMap,
    ApiPath(path): ApiPath<crate::lexicon::dto::EntryPath>,
    ApiJson(input): ApiJson<CreateContentCompletionJobInput>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    if state.lexicon_content_generator.is_none() {
        return Err(AppError::unavailable(
            ErrorCode::ServiceUnavailable,
            "lexicon content generator is not configured",
        ));
    }
    validate_create(&input)
        .map_err(|detail| AppError::unprocessable(ErrorCode::ValidationFailed, detail))?;
    let key = super::super::handler::required_idempotency_key(&headers)
        .map_err(super::super::handler::idempotency_key_error)?;
    let response = ContentCompletionRepository::new(state.pool.clone())
        .create(admin.id, key, path.id, &input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/entries/{id}/content-completion-jobs/{job_id}",
    tag = "admin-lexicon", security(("bearer_auth" = [])), params(ContentCompletionJobPath),
    responses(
        (status = 200, description = "生成任务、分区和候选内容", body = ContentCompletionJobEnvelope),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "任务不存在或不属于当前管理员/词条")
    )
)]
pub async fn get_content_completion_job(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<ContentCompletionJobPath>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let response = ContentCompletionRepository::new(state.pool.clone())
        .get(admin.id, path.id, path.job_id)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/{id}/content-completion-jobs/{job_id}/retries",
    tag = "admin-lexicon", security(("bearer_auth" = [])),
    params(ContentCompletionJobPath, ("Idempotency-Key" = uuid::Uuid, Header, description = "失败分区重试幂等键（UUID）")),
    request_body = RetryContentCompletionJobInput,
    responses(
        (status = 202, description = "失败或缺失分区已重新排队", body = ContentCompletionJobEnvelope),
        (status = 400, description = "Idempotency-Key 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "任务不存在"),
        (status = 409, description = "幂等键冲突"),
        (status = 422, description = "分区为空、未知或并非失败状态"),
        (status = 503, description = "真实生成提供方未配置")
    )
)]
pub async fn retry_content_completion_job(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    ApiPath(path): ApiPath<ContentCompletionJobPath>,
    ApiJson(input): ApiJson<RetryContentCompletionJobInput>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    if state.lexicon_content_generator.is_none() {
        return Err(AppError::unavailable(
            ErrorCode::ServiceUnavailable,
            "lexicon content generator is not configured",
        ));
    }
    let key = super::super::handler::required_idempotency_key(&headers)
        .map_err(super::super::handler::idempotency_key_error)?;
    let response = ContentCompletionRepository::new(state.pool.clone())
        .retry(admin.id, key, path.id, path.job_id, &input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

fn validate_create(input: &CreateContentCompletionJobInput) -> Result<(), &'static str> {
    if input.base_revision < 1 {
        return Err("base_revision must be positive");
    }
    let unique = input.scope.iter().collect::<std::collections::HashSet<_>>();
    if input.scope.len() != 3 || unique.len() != 3 {
        return Err("scope must contain grammar_structures, meanings, and examples exactly once");
    }
    Ok(())
}

fn map_error(error: ContentCompletionRepositoryError) -> AppError {
    match error {
        ContentCompletionRepositoryError::WordNotFound
        | ContentCompletionRepositoryError::JobNotFound => AppError::not_found_with_code(
            ErrorCode::WordNotFound,
            "word or completion job not found",
        ),
        ContentCompletionRepositoryError::EntryArchived => AppError::unprocessable(
            ErrorCode::EntryArchived,
            "archived entries cannot generate content",
        ),
        ContentCompletionRepositoryError::RevisionConflict(current) => AppError::conflict(
            ErrorCode::RevisionConflict,
            Some("base_revision"),
            "revision conflict",
        )
        .with_meta(ProblemMeta {
            current_revision: Some(current),
            ..ProblemMeta::default()
        }),
        ContentCompletionRepositoryError::IdempotencyConflict => AppError::conflict(
            ErrorCode::IdempotencyConflict,
            None,
            "idempotency key was already used with another request",
        ),
        ContentCompletionRepositoryError::InvalidRetry => AppError::unprocessable(
            ErrorCode::ValidationFailed,
            "completion scope or retry partitions are invalid",
        ),
        other => AppError::internal(other),
    }
}
