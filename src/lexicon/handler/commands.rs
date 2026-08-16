use super::*;

// --- creation ---

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/detections",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    request_body = DetectWordInputV2,
    responses(
        (status = 200, description = "内置词典匹配与智能词库查重结果", body = DetectWordResponseV2),
        (status = 400, description = "词头非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 422, description = "语言不受支持或请求结构非法"),
        (status = 503, description = "检测上下文存储不可用")
    )
)]
pub async fn detect(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<DetectWordInputV2>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .detect(admin.id, input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(("Idempotency-Key" = Uuid, Header, description = "创建命令幂等键（UUID）")),
    request_body = CreateAdminWordV2Input,
    responses(
        (status = 201, description = "V2 词条草稿创建成功", body = AdminWordV2Envelope),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 409, description = "词头重复或幂等键冲突"),
        (status = 410, description = "检测上下文已过期"),
        (status = 422, description = "检测结果与创建请求不匹配"),
        (status = 503, description = "检测上下文存储不可用")
    )
)]
pub async fn create(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<CreateAdminWordV2Input>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let idempotency_key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    let response = service(&state)
        .create(admin.id, request_id.as_uuid(), idempotency_key, input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::CREATED, Json(response)))
}

// --- dialect suggestion ---

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/dialect-variant-suggestions",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    request_body = SuggestDialectVariantsInputV2,
    responses(
        (status = 200, description = "基于当前内置词典地区证据的确定性方言建议", body = SuggestDialectVariantsResponseV2),
        (status = 400, description = "请求 JSON 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 422, description = "方言方向、项目或 RichText 非法")
    )
)]
pub async fn suggest_dialect_variants(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<SuggestDialectVariantsInputV2>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .suggest_dialect_variants(input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

// --- editor ---

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/{id}/steps/forms/impact",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    request_body = PreviewFormsImpactInputV2,
    responses(
        (status = 200, description = "词形 surface warning、下游影响与独立确认 token", body = FormsImpactResponseV2),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision、surface warning 或策略冲突"),
        (status = 422, description = "词形结构非法"),
        (status = 503, description = "确认 token 存储不可用")
    )
)]
pub async fn preview_forms_impact(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<PreviewFormsImpactInputV2>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .preview_forms_impact(admin.id, path.id, input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/lexicon/entries/{id}/steps/forms",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    request_body = SaveFormsStepInput,
    responses(
        (status = 200, description = "保存或完成词形与发音步骤", body = AdminWordV2Envelope),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision、surface warning、策略或下游确认冲突"),
        (status = 410, description = "surface 确认 snapshot 已过期"),
        (status = 422, description = "词形校验失败"),
        (status = 503, description = "确认 token 存储不可用")
    )
)]
pub async fn save_forms(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<SaveFormsStepInput>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .save_forms(admin.id, request_id.as_uuid(), path.id, input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/lexicon/entries/{id}/steps/meanings",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    request_body = SaveMeaningsStepInput,
    responses(
        (status = 200, description = "保存或完成词义与例句步骤", body = AdminWordV2Envelope),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision 或步骤可达性冲突"),
        (status = 422, description = "词义校验失败")
    )
)]
pub async fn save_meanings(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<SaveMeaningsStepInput>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .save_meanings(admin.id, request_id.as_uuid(), path.id, input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/{id}/validate",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    request_body = ValidateAdminWordV2Input,
    responses(
        (status = 200, description = "指定 revision 的发布完整性校验", body = DraftValidationResponse),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision 冲突")
    )
)]
pub async fn validate(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<ValidateAdminWordV2Input>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .validate(path.id, input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/{id}/publications",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(
        EntryPath,
        ("Idempotency-Key" = Uuid, Header, description = "发布命令幂等键（UUID）")
    ),
    request_body = PublishAdminWordV2Input,
    responses(
        (status = 201, description = "发布不可变词条版本", body = AdminWordV2Envelope),
        (status = 400, description = "缺少或错误的 Idempotency-Key"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision、surface、policy、visibility 或幂等键冲突"),
        (status = 410, description = "surface 确认 snapshot 已过期"),
        (status = 422, description = "发布完整性校验失败"),
        (status = 503, description = "surface 确认服务不可用")
    )
)]
pub async fn publish(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<PublishAdminWordV2Input>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let idempotency_key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    let response = service(&state)
        .publish(
            admin.id,
            request_id.as_uuid(),
            path.id,
            idempotency_key,
            input,
        )
        .await
        .map_err(map_error)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/{id}/publications/{publication_id}/activate",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(
        PublicationPath,
        ("Idempotency-Key" = Uuid, Header, description = "历史 publication activation 命令幂等键（UUID）")
    ),
    request_body = ActivatePublicationInput,
    responses(
        (status = 200, description = "指定历史 publication 已切换为当前公开版本", body = AdminWordV2Envelope),
        (status = 400, description = "路径、header 或 JSON 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用、必须先改密或词条已归档"),
        (status = 404, description = "词条或 publication 不存在"),
        (status = 409, description = "revision、surface、policy、visibility 或幂等键冲突"),
        (status = 410, description = "surface 确认 snapshot 已过期"),
        (status = 422, description = "revision 取值非法"),
        (status = 503, description = "surface 确认服务不可用")
    )
)]
pub async fn activate_publication(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    ApiPath(path): ApiPath<PublicationPath>,
    ApiJson(input): ApiJson<ActivatePublicationInput>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let idempotency_key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    let response = service(&state)
        .activate_publication(
            admin.id,
            request_id.as_uuid(),
            path.id,
            path.publication_id,
            idempotency_key,
            input,
        )
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}
