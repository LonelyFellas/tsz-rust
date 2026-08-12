use super::*;

#[utoipa::path(
    delete,
    path = "/api/v1/admin/lexicon/entries/{id}",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    request_body = DeleteDraftInput,
    responses(
        (status = 204, description = "从未发布的草稿已永久删除并释放词头"),
        (status = 400, description = "路径非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision 冲突，或词条已有发布历史/被其他草稿引用"),
        (status = 422, description = "base_revision 取值非法")
    )
)]
pub async fn delete_draft(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<DeleteDraftInput>,
) -> Result<StatusCode, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    service(&state)
        .delete_draft(admin.id, request_id.as_uuid(), path.id, input)
        .await
        .map_err(map_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/{id}/archive",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath, ("Idempotency-Key" = Uuid, Header, description = "归档命令幂等键（UUID）")),
    request_body = EntryLifecycleInput,
    responses(
        (status = 200, description = "词条已归档且 publication 历史保持不变", body = AdminWordV2Envelope),
        (status = 400, description = "路径、header 或 JSON 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision、幂等键或当前入站引用冲突"),
        (status = 422, description = "revision 取值非法")
    )
)]
pub async fn archive(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<EntryLifecycleInput>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    let response = service(&state)
        .archive(admin.id, request_id.as_uuid(), path.id, key, input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/{id}/restore",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath, ("Idempotency-Key" = Uuid, Header, description = "恢复命令幂等键（UUID）")),
    request_body = EntryLifecycleInput,
    responses(
        (status = 200, description = "词条已恢复且 publication 历史保持不变", body = AdminWordV2Envelope),
        (status = 400, description = "路径、header 或 JSON 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision 或幂等键冲突"),
        (status = 422, description = "revision 取值非法")
    )
)]
pub async fn restore(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<EntryLifecycleInput>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    let response = service(&state)
        .restore(admin.id, request_id.as_uuid(), path.id, key, input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/archive-batch",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(("Idempotency-Key" = Uuid, Header, description = "批量归档命令幂等键（UUID）")),
    request_body = EntryLifecycleBatchInput,
    responses(
        (status = 200, description = "原子批量归档结果", body = EntryLifecycleBatchResponse),
        (status = 400, description = "header 或 JSON 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "任一词条不存在"),
        (status = 409, description = "任一 revision、幂等键或当前入站引用冲突"),
        (status = 422, description = "批量为空、重复、超限或 revision 非法")
    )
)]
pub async fn archive_batch(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<EntryLifecycleBatchInput>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    let response = service(&state)
        .archive_batch(admin.id, request_id.as_uuid(), key, input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/restore-batch",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(("Idempotency-Key" = Uuid, Header, description = "批量恢复命令幂等键（UUID）")),
    request_body = EntryLifecycleBatchInput,
    responses(
        (status = 200, description = "原子批量恢复结果", body = EntryLifecycleBatchResponse),
        (status = 400, description = "header 或 JSON 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "任一词条不存在"),
        (status = 409, description = "任一 revision 或幂等键冲突"),
        (status = 422, description = "批量为空、重复、超限或 revision 非法")
    )
)]
pub async fn restore_batch(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<EntryLifecycleBatchInput>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    let response = service(&state)
        .restore_batch(admin.id, request_id.as_uuid(), key, input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}
