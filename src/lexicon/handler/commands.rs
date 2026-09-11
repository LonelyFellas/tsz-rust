use super::*;

// --- creation ---

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/detections",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    request_body = DetectLexiconSurfaceV3Input,
    responses(
        (status = 200, description = "form surface 检测结果", body = DetectLexiconSurfaceResponseV3),
        (status = 400, description = "词头非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 422, description = "语言不受支持、请求结构非法或 schema_version 不是 3"),
        (status = 503, description = "检测上下文存储不可用，或 V3 surface projection 能力尚未实现")
    )
)]
pub async fn detect(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<Value>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    v3_contract::require_schema_version_3(&input)?;
    let input: DetectLexiconSurfaceV3Input = v3_contract::decode_request(input)?;
    if !state.smart_lexicon_v3_flags.projection {
        return Err(v3_detection_unavailable());
    }
    let response = service(&state)
        .detect_v3(admin.id, input)
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
    request_body = CreateAdminWordV3Input,
    responses(
        (status = 201, description = "词条草稿创建成功", body = AdminWordV3Envelope),
        (status = 400, description = "主词为空、过长、含控制字符，或不是英文词条（含非拉丁字符或不含字母）"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用、必须先改密，或 annotation_updates 里带了非超管无权改的他人词条"),
        (status = 409, description = "词头重复或幂等键冲突"),
        (status = 410, description = "检测上下文已过期"),
        (status = 422, description = "请求结构非法、检测上下文不匹配、词典不可用或 schema_version 不是 3"),
        (status = 503, description = "检测上下文存储不可用")
    )
)]
pub async fn create(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<Value>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let idempotency_key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    v3_contract::require_schema_version_3(&input)?;
    let input: CreateAdminWordV3Input = v3_contract::decode_request(input)?;
    if !state.smart_lexicon_v3_flags.create || !state.smart_lexicon_v3_flags.projection {
        return Err(v3_storage_unavailable());
    }
    let response = service(&state)
        .create_v3(
            admin.id,
            admin.is_super_admin(),
            request_id.as_uuid(),
            idempotency_key,
            input,
            state.smart_lexicon_v3_flags.projection,
        )
        .await
        .map_err(map_error)?;
    Ok((StatusCode::CREATED, Json(response)))
}

// --- editor ---

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/{id}/steps/forms/impact",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    request_body = PreviewFormsImpactInputV3,
    responses(
        (status = 200, description = "词形 surface warning、下游影响与独立确认 token", body = FormsImpactResponseV3),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision、stable_node_id_changed、form_reference_conflict、surface warning 或策略冲突"),
        (status = 413, description = "请求体超过 8,192,000 字节"),
        (status = 422, description = "词形结构非法或 schema_version 不是 3"),
        (status = 503, description = "确认 token 或 V3 存储能力不可用")
    )
)]
pub async fn preview_forms_impact(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<Value>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    v3_contract::require_schema_version_3(&input)?;
    let input: PreviewFormsImpactInputV3 = v3_contract::decode_v3_forms_request(input)?;
    v3_contract::require_positive_revision("base_revision", input.base_revision)?;
    let issues = v3_contract::validate_forms(&input.content, StepSaveIntent::Save);
    if !issues.is_empty() {
        return Err(v3_contract::contract_validation_error(&issues));
    }
    if !state.smart_lexicon_v3_flags.edit || !state.smart_lexicon_v3_flags.projection {
        return Err(v3_storage_unavailable());
    }
    let response = service(&state)
        .preview_forms_impact_v3(
            admin.id,
            path.id,
            input,
            state.smart_lexicon_v3_flags.projection,
        )
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
    request_body = SaveFormsStepInputV3,
    responses(
        (status = 200, description = "保存或完成词形与发音步骤", body = AdminWordV3Envelope),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用、必须先改密，或非超管操作他人的未发布草稿"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision、stable_node_id_changed、form_reference_conflict、surface warning、策略或下游确认冲突"),
        (status = 410, description = "surface 确认 snapshot 已过期"),
        (status = 413, description = "请求体超过 8,192,000 字节"),
        (status = 422, description = "词形校验失败或 schema_version 不是 3"),
        (status = 503, description = "确认 token 或 V3 存储能力不可用")
    )
)]
pub async fn save_forms(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<Value>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    v3_contract::require_schema_version_3(&input)?;
    let input: SaveFormsStepInputV3 = v3_contract::decode_v3_forms_request(input)?;
    v3_contract::require_positive_revision("base_revision", input.base_revision)?;
    let issues = v3_contract::validate_forms(&input.content, input.intent);
    if !issues.is_empty() {
        return Err(v3_contract::contract_validation_error(&issues));
    }
    if !state.smart_lexicon_v3_flags.edit || !state.smart_lexicon_v3_flags.projection {
        return Err(v3_storage_unavailable());
    }
    let mut response = service(&state)
        .save_forms_v3(
            admin.id,
            request_id.as_uuid(),
            path.id,
            input,
            state.smart_lexicon_v3_flags.projection,
            admin.is_super_admin(),
        )
        .await
        .map_err(map_error)?;
    apply_capability_flags(
        &mut response.word.capabilities,
        state.smart_lexicon_v3_flags,
    );
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/lexicon/entries/{id}/steps/meanings",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    request_body = SaveMeaningsStepInputV3,
    responses(
        (status = 200, description = "保存或完成词义与例句步骤", body = AdminWordV3Envelope),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用、必须先改密，或非超管操作他人的未发布草稿"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision 或步骤可达性冲突"),
        (status = 413, description = "请求体超过 8,192,000 字节"),
        (status = 422, description = "词义校验失败或 schema_version 不是 3"),
        (status = 503, description = "V3 存储能力不可用")
    )
)]
pub async fn save_meanings(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<Value>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    v3_contract::require_schema_version_3(&input)?;
    let input: SaveMeaningsStepInputV3 = v3_contract::decode_v3_meanings_request(input)?;
    v3_contract::require_positive_revision("base_revision", input.base_revision)?;
    let issues = v3_contract::validate_meanings(&input.content, input.intent);
    if !issues.is_empty() {
        return Err(v3_contract::contract_validation_error(&issues));
    }
    if !state.smart_lexicon_v3_flags.edit || !state.smart_lexicon_v3_flags.projection {
        return Err(v3_storage_unavailable());
    }
    let mut response = service(&state)
        .save_meanings_v3(
            admin.id,
            request_id.as_uuid(),
            path.id,
            input,
            admin.is_super_admin(),
        )
        .await
        .map_err(map_error)?;
    apply_capability_flags(
        &mut response.word.capabilities,
        state.smart_lexicon_v3_flags,
    );
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/{id}/validate",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    request_body = ValidateAdminWordV3Input,
    responses(
        (status = 200, description = "指定 revision 的发布完整性校验", body = DraftValidationResponseV3),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision 冲突"),
        (status = 422, description = "schema_version 不是 3"),
        (status = 503, description = "V3 存储能力不可用")
    )
)]
pub async fn validate(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<Value>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    v3_contract::require_schema_version_3(&input)?;
    let input: ValidateAdminWordV3Input = v3_contract::decode_request(input)?;
    v3_contract::require_positive_revision("base_revision", input.base_revision)?;
    if !state.smart_lexicon_v3_flags.read {
        return Err(v3_storage_unavailable());
    }
    let response = service(&state)
        .validate_v3(path.id, input)
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
    request_body = PublishAdminWordV3Input,
    responses(
        (status = 201, description = "发布不可变词条版本", body = AdminWordV3Envelope),
        (status = 400, description = "缺少或错误的 Idempotency-Key"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用、必须先改密，或非超管操作他人的未发布草稿"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision、surface、policy、visibility 或幂等键冲突"),
        (status = 410, description = "surface 确认 snapshot 已过期"),
        (status = 422, description = "发布完整性校验失败或 schema_version 不是 3"),
        (status = 503, description = "surface 确认服务不可用")
    )
)]
pub async fn publish(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<Value>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let idempotency_key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    v3_contract::require_schema_version_3(&input)?;
    let input: PublishAdminWordV3Input = v3_contract::decode_request(input)?;
    v3_contract::require_positive_revision("base_revision", input.base_revision)?;
    if !state.smart_lexicon_v3_flags.publish {
        return Err(v3_storage_unavailable());
    }
    if !state.smart_lexicon_v3_flags.projection {
        return Err(v3_storage_unavailable());
    }
    let mut response = service(&state)
        .publish_v3(
            admin.id,
            request_id.as_uuid(),
            path.id,
            idempotency_key,
            input,
            !sentence_target_discovery_enabled(state.smart_lexicon_v3_flags),
            admin.is_super_admin(),
        )
        .await
        .map_err(map_error)?;
    apply_capability_flags(
        &mut response.word.capabilities,
        state.smart_lexicon_v3_flags,
    );
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
    request_body = ActivatePublicationV3Input,
    responses(
        (status = 200, description = "指定历史 publication 已切换为当前公开版本", body = AdminWordV3Envelope),
        (status = 400, description = "路径、header 或 JSON 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用、必须先改密或词条已归档"),
        (status = 404, description = "词条或 publication 不存在"),
        (status = 409, description = "revision、surface、policy、visibility 或幂等键冲突"),
        (status = 410, description = "surface 确认 snapshot 已过期"),
        (status = 422, description = "revision 取值非法或 schema_version 不是 3"),
        (status = 503, description = "surface 确认服务不可用")
    )
)]
pub async fn activate_publication(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    ApiPath(path): ApiPath<PublicationPath>,
    ApiJson(input): ApiJson<Value>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let idempotency_key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    v3_contract::require_schema_version_3(&input)?;
    let input: ActivatePublicationV3Input = v3_contract::decode_request(input)?;
    v3_contract::require_positive_revision("base_revision", input.base_revision)?;
    v3_contract::require_positive_revision(
        "base_lifecycle_revision",
        input.base_lifecycle_revision,
    )?;
    if !state.smart_lexicon_v3_flags.publish {
        return Err(v3_storage_unavailable());
    }
    if !state.smart_lexicon_v3_flags.projection {
        return Err(v3_storage_unavailable());
    }
    let mut response = service(&state)
        .activate_publication_v3(
            admin.id,
            request_id.as_uuid(),
            path.id,
            path.publication_id,
            idempotency_key,
            input,
        )
        .await
        .map_err(map_error)?;
    apply_capability_flags(
        &mut response.word.capabilities,
        state.smart_lexicon_v3_flags,
    );
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    patch, path = "/api/v1/admin/lexicon/entries/{id}/annotation", tag = "admin-lexicon",
    security(("bearer_auth" = [])), params(EntryPath),
    request_body = crate::lexicon::dto::UpdateEntryAnnotationInput,
    responses(
        (status = 200, description = "标注已保存，内容修订不变", body = crate::lexicon::dto::EntryAnnotationResponse),
        (status = 400, description = "标注或修订非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号不可编辑，或非超管改他人创建的词条"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "标注组、修订冲突或词条已归档")
    )
)]
pub async fn update_annotation(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    ApiPath(path): ApiPath<EntryPath>,
    ApiJson(input): ApiJson<crate::lexicon::dto::UpdateEntryAnnotationInput>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .update_annotation(
            admin.id,
            admin.is_super_admin(),
            request_id.as_uuid(),
            path.id,
            input,
        )
        .await
        .map_err(map_error)?;
    Ok(Json(response))
}
