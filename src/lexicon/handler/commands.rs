use super::*;

// --- creation ---

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/detections",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    request_body = DetectLexiconInputAny,
    responses(
        (status = 200, description = "按 schema_version 判别的检测结果；V2 为 legacy headword，V3 为 form surface", body = DetectLexiconResponseAny),
        (status = 400, description = "词头非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 422, description = "语言不受支持或请求结构非法"),
        (status = 503, description = "检测上下文存储不可用，或 V3 surface projection 能力尚未实现")
    )
)]
pub async fn detect(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<Value>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    match v3_contract::request_schema_version_or_legacy(&input)? {
        Some(3) => {
            let input: DetectLexiconSurfaceV3Input = v3_contract::decode_request(input)?;
            if !state.smart_lexicon_v3_flags.projection {
                return Err(v3_detection_unavailable());
            }
            let response = service(&state)
                .detect_v3(admin.id, input)
                .await
                .map_err(map_error)?;
            return Ok((
                StatusCode::OK,
                Json(DetectLexiconResponseAny::V3(Box::new(response))),
            ));
        }
        Some(2) => {
            return Err(AppError::unprocessable(
                ErrorCode::InvalidRequestBody,
                "legacy V2 detection bodies omit schema_version",
            ));
        }
        None => {}
        Some(_) => unreachable!("request_schema_version filters unsupported versions"),
    }
    let input: DetectWordInputV2 = v3_contract::decode_request(input)?;
    let response = service(&state)
        .detect(admin.id, input)
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::OK,
        Json(DetectLexiconResponseAny::V2(Box::new(response))),
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(("Idempotency-Key" = Uuid, Header, description = "创建命令幂等键（UUID）")),
    request_body = CreateAdminWordAnyInput,
    responses(
        (status = 201, description = "版本化词条草稿创建成功", body = AdminWordAnyEnvelope),
        (status = 400, description = "主词为空、过长、含控制字符，或不是英文词条（含非拉丁字符或不含字母）"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 409, description = "词头重复或幂等键冲突"),
        (status = 410, description = "检测上下文已过期"),
        (status = 422, description = "请求结构非法、检测上下文不匹配或词典不可用"),
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
    match v3_contract::request_schema_version_or_legacy(&input)? {
        Some(3) => {
            let input: CreateAdminWordV3Input = v3_contract::decode_request(input)?;
            if !state.smart_lexicon_v3_flags.create || !state.smart_lexicon_v3_flags.projection {
                return Err(v3_storage_unavailable());
            }
            let response = service(&state)
                .create_v3(
                    admin.id,
                    request_id.as_uuid(),
                    idempotency_key,
                    input,
                    state.smart_lexicon_v3_flags.projection,
                )
                .await
                .map_err(map_error)?;
            return Ok((StatusCode::CREATED, Json(response)));
        }
        Some(2) | None => {}
        Some(_) => unreachable!("request_schema_version filters unsupported versions"),
    }
    let input: CreateAdminWordV2Input = v3_contract::decode_request(input)?;
    let response = service(&state)
        .create(admin.id, request_id.as_uuid(), idempotency_key, input)
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::CREATED,
        Json(AdminWordAnyEnvelope {
            word: response.word.into(),
        }),
    ))
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
    request_body = PreviewFormsImpactInputAny,
    responses(
        (status = 200, description = "词形 surface warning、下游影响与独立确认 token", body = FormsImpactResponseAny),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision、stable_node_id_changed、form_reference_conflict、surface warning 或策略冲突"),
        (status = 413, description = "请求体超过 8,192,000 字节"),
        (status = 422, description = "词形结构非法"),
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
    match v3_contract::request_schema_version_or_legacy(&input)? {
        Some(3) => {
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
            return Ok((StatusCode::OK, Json(FormsImpactResponseAny::V3(response))));
        }
        Some(2) | None => {}
        Some(_) => unreachable!("request_schema_version filters unsupported versions"),
    }
    let input: PreviewFormsImpactInputV2 = v3_contract::decode_request(input)?;
    let response = service(&state)
        .preview_forms_impact(admin.id, path.id, input)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(FormsImpactResponseAny::V2(response))))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/lexicon/entries/{id}/steps/forms",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    request_body = SaveFormsStepInputAny,
    responses(
        (status = 200, description = "保存或完成版本化词形与发音步骤", body = AdminWordAnyEnvelope),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision、stable_node_id_changed、form_reference_conflict、surface warning、策略或下游确认冲突"),
        (status = 410, description = "surface 确认 snapshot 已过期"),
        (status = 413, description = "请求体超过 8,192,000 字节"),
        (status = 422, description = "词形校验失败"),
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
    match v3_contract::request_schema_version_or_legacy(&input)? {
        Some(3) => {
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
                )
                .await
                .map_err(map_error)?;
            apply_legacy_bridge_read_flag(
                &mut response,
                state.smart_lexicon_v3_flags.legacy_bridge_read,
            );
            apply_sentence_association_flag(&mut response, state.smart_lexicon_v3_flags);
            return Ok((StatusCode::OK, Json(response)));
        }
        Some(2) | None => {}
        Some(_) => unreachable!("request_schema_version filters unsupported versions"),
    }
    let input: SaveFormsStepInput = v3_contract::decode_request(input)?;
    let response = service(&state)
        .save_forms(admin.id, request_id.as_uuid(), path.id, input)
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::OK,
        Json(AdminWordAnyEnvelope {
            word: response.word.into(),
        }),
    ))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/lexicon/entries/{id}/steps/meanings",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    request_body = SaveMeaningsStepInputAny,
    responses(
        (status = 200, description = "保存或完成版本化词义与例句步骤", body = AdminWordAnyEnvelope),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision 或步骤可达性冲突"),
        (status = 413, description = "请求体超过 8,192,000 字节"),
        (status = 422, description = "词义校验失败"),
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
    match v3_contract::request_schema_version_or_legacy(&input)? {
        Some(3) => {
            let input: SaveMeaningsStepInputV3 = v3_contract::decode_v3_meanings_request(input)?;
            v3_contract::require_positive_revision("base_revision", input.base_revision)?;
            if !draft_relation_prebinding_enabled(state.smart_lexicon_v3_flags)
                && input
                    .content
                    .pos
                    .iter()
                    .flat_map(|pos| &pos.senses)
                    .flat_map(|sense| &sense.relations)
                    .any(|relation| relation.prebound_target_word_id.is_some())
            {
                return Err(v3_storage_unavailable());
            }
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
                    draft_relation_prebinding_enabled(state.smart_lexicon_v3_flags),
                )
                .await
                .map_err(map_error)?;
            apply_legacy_bridge_read_flag(
                &mut response,
                state.smart_lexicon_v3_flags.legacy_bridge_read,
            );
            apply_sentence_association_flag(&mut response, state.smart_lexicon_v3_flags);
            return Ok((StatusCode::OK, Json(response)));
        }
        Some(2) | None => {}
        Some(_) => unreachable!("request_schema_version filters unsupported versions"),
    }
    let input: SaveMeaningsStepInput = v3_contract::decode_request(input)?;
    let response = service(&state)
        .save_meanings(admin.id, request_id.as_uuid(), path.id, input)
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::OK,
        Json(AdminWordAnyEnvelope {
            word: response.word.into(),
        }),
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/{id}/validate",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    request_body = ValidateAdminWordAnyInput,
    responses(
        (status = 200, description = "指定 revision 的版本化发布完整性校验", body = DraftValidationResponseAny),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 409, description = "revision 冲突"),
        (status = 422, description = "schema_version 不受支持"),
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
    match v3_contract::request_schema_version_or_legacy(&input)? {
        Some(3) => {
            let input: ValidateAdminWordV3Input = v3_contract::decode_request(input)?;
            v3_contract::require_positive_revision("base_revision", input.base_revision)?;
            if !state.smart_lexicon_v3_flags.read {
                return Err(v3_storage_unavailable());
            }
            let response = service(&state)
                .validate_v3(path.id, input)
                .await
                .map_err(map_error)?;
            return Ok((
                StatusCode::OK,
                Json(DraftValidationResponseAny::V3(response)),
            ));
        }
        Some(2) | None => {}
        Some(_) => unreachable!("request_schema_version filters unsupported versions"),
    }
    let input: ValidateAdminWordV2Input = v3_contract::decode_request(input)?;
    let response = service(&state)
        .validate(path.id, input)
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::OK,
        Json(DraftValidationResponseAny::V2(response)),
    ))
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
    request_body = PublishAdminWordAnyInput,
    responses(
        (status = 201, description = "发布不可变版本化词条", body = AdminWordAnyEnvelope),
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
    ApiJson(input): ApiJson<Value>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let idempotency_key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    match v3_contract::request_schema_version_or_legacy(&input)? {
        Some(3) => {
            let input: PublishAdminWordV3Input = v3_contract::decode_request(input)?;
            v3_contract::require_positive_revision("base_revision", input.base_revision)?;
            if !state.smart_lexicon_v3_flags.publish {
                return Err(v3_publication_requires_migration_canary());
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
                )
                .await
                .map_err(map_error)?;
            apply_legacy_bridge_read_flag(
                &mut response,
                state.smart_lexicon_v3_flags.legacy_bridge_read,
            );
            apply_sentence_association_flag(&mut response, state.smart_lexicon_v3_flags);
            return Ok((StatusCode::CREATED, Json(response)));
        }
        Some(2) | None => {}
        Some(_) => unreachable!("request_schema_version filters unsupported versions"),
    }
    let input: PublishAdminWordV2Input = v3_contract::decode_request(input)?;
    let response = service(&state)
        .publish(
            admin.id,
            request_id.as_uuid(),
            path.id,
            idempotency_key,
            input,
            state.smart_lexicon_v3_flags.read && state.smart_lexicon_v3_flags.projection,
            !sentence_target_discovery_enabled(state.smart_lexicon_v3_flags),
        )
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::CREATED,
        Json(AdminWordAnyEnvelope {
            word: response.word.into(),
        }),
    ))
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
    request_body = ActivatePublicationAnyInput,
    responses(
        (status = 200, description = "指定历史 publication 已切换为当前公开版本", body = AdminWordAnyEnvelope),
        (status = 400, description = "路径、header 或 JSON 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用、必须先改密或词条已归档"),
        (status = 404, description = "词条或 publication 不存在"),
        (status = 409, description = "revision、surface、policy、visibility、幂等键冲突，或 V3 publication 未通过服务端迁移 canary 白名单"),
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
    ApiJson(input): ApiJson<Value>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let idempotency_key = required_idempotency_key(&headers).map_err(idempotency_key_error)?;
    match v3_contract::request_schema_version_or_legacy(&input)? {
        Some(3) => {
            let input: ActivatePublicationV3Input = v3_contract::decode_request(input)?;
            v3_contract::require_positive_revision("base_revision", input.base_revision)?;
            v3_contract::require_positive_revision(
                "base_lifecycle_revision",
                input.base_lifecycle_revision,
            )?;
            if !state.smart_lexicon_v3_flags.publish {
                return Err(v3_publication_requires_migration_canary());
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
            apply_legacy_bridge_read_flag(
                &mut response,
                state.smart_lexicon_v3_flags.legacy_bridge_read,
            );
            apply_sentence_association_flag(&mut response, state.smart_lexicon_v3_flags);
            return Ok((StatusCode::OK, Json(response)));
        }
        Some(2) | None => {}
        Some(_) => unreachable!("request_schema_version filters unsupported versions"),
    }
    let input: ActivatePublicationInput = v3_contract::decode_request(input)?;
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
    Ok((
        StatusCode::OK,
        Json(AdminWordAnyEnvelope {
            word: response.word.into(),
        }),
    ))
}
