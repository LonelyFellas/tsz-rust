use super::*;

// --- entry query ---

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/entries/sentence-targets/resolve",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    request_body = ResolveSentenceTargetsV3Input,
    responses(
        (status = 200, description = "发现句中已发布词条目标及手选草稿候选", body = ResolveSentenceTargetsV3Response),
        (status = 400, description = "JSON 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 422, description = "正文、片段或分页参数非法"),
        (status = 503, description = "V3 发现能力未开启")
    )
)]
pub async fn resolve_sentence_targets(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<ResolveSentenceTargetsV3Input>,
) -> Result<(StatusCode, Json<ResolveSentenceTargetsV3Response>), AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .resolve_sentence_targets_v3(
            input,
            sentence_target_discovery_enabled(state.smart_lexicon_v3_flags),
        )
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/entries/{id}",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    responses(
        (status = 200, description = "版本化 canonical 词条草稿", body = AdminWordDraftAnyEnvelope),
        (status = 400, description = "词条 ID 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 422, description = "词条 schema_version 不受当前 reader 支持"),
        (status = 500, description = "数据库查询失败")
    )
)]
pub async fn get(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let mut response = service(&state)
        .get_draft_any(path.id)
        .await
        .map_err(map_error)?;
    if matches!(&response, AdminWordDraftAnyEnvelope::V3(_)) && !state.smart_lexicon_v3_flags.read {
        return Err(v3_storage_unavailable());
    }
    apply_draft_legacy_bridge_read_flag(
        &mut response,
        state.smart_lexicon_v3_flags.legacy_bridge_read,
    );
    apply_draft_sentence_association_flag(
        &mut response,
        sentence_association_enabled(state.smart_lexicon_v3_flags),
        sentence_target_discovery_enabled(state.smart_lexicon_v3_flags),
        draft_relation_prebinding_enabled(state.smart_lexicon_v3_flags),
    );
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/entries/{id}/pending-sentence-associations",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath, PendingSentenceAssociationListQuery),
    responses(
        (status = 200, description = "当前目标词条可认领的 Pending 例句关联", body = PendingSentenceAssociationListResponse),
        (status = 400, description = "词条 ID、cursor 或 page_size 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "目标词条不存在、已归档或尚未发布"),
        (status = 503, description = "V3 读取、编辑或投影能力未开启")
    )
)]
pub async fn list_pending_sentence_associations(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
    ApiQuery(query): ApiQuery<PendingSentenceAssociationListQuery>,
) -> Result<(StatusCode, Json<PendingSentenceAssociationListResponse>), AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .pending_sentence_associations(
            path.id,
            query,
            state.smart_lexicon_v3_flags.read
                && state.smart_lexicon_v3_flags.edit
                && state.smart_lexicon_v3_flags.projection
                && state.smart_lexicon_v3_flags.sentence_associations,
        )
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/entries/{id}/publications",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    responses(
        (status = 200, description = "不可变 publication 历史，按各 snapshot 自身 schema_version 判别", body = AdminWordPublicationListResponse),
        (status = 400, description = "词条 ID 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 422, description = "历史 snapshot schema_version 不受当前 reader 支持"),
        (status = 500, description = "数据库查询或 snapshot 解码失败")
    )
)]
pub async fn list_publications(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
) -> Result<(StatusCode, Json<AdminWordPublicationListResponse>), AppError> {
    require_active_admin(&state, &auth).await?;
    let mut response = service(&state)
        .publication_history(path.id, state.smart_lexicon_v3_flags.read)
        .await
        .map_err(map_error)?;
    for publication in &mut response.publications {
        apply_publication_legacy_bridge_read_flag(
            publication,
            state.smart_lexicon_v3_flags.legacy_bridge_read,
        );
        apply_publication_sentence_association_flag(
            publication,
            sentence_association_enabled(state.smart_lexicon_v3_flags),
            sentence_target_discovery_enabled(state.smart_lexicon_v3_flags),
            draft_relation_prebinding_enabled(state.smart_lexicon_v3_flags),
        );
    }
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/entries/{id}/publications/{publication_id}",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(PublicationPath),
    responses(
        (status = 200, description = "按 snapshot 自身 schema_version 判别的不可变 publication", body = AdminWordPublicationEnvelope),
        (status = 400, description = "词条或 publication ID 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "publication 不存在"),
        (status = 422, description = "历史 snapshot schema_version 不受当前 reader 支持"),
        (status = 503, description = "V3 reader 能力未启用"),
        (status = 500, description = "数据库查询或 snapshot 解码失败")
    )
)]
pub async fn get_publication(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<PublicationPath>,
) -> Result<(StatusCode, Json<AdminWordPublicationEnvelope>), AppError> {
    require_active_admin(&state, &auth).await?;
    let mut response = service(&state)
        .publication(path.id, path.publication_id)
        .await
        .map_err(map_error)?;
    if matches!(&response.publication, AdminWordPublicationAny::V3(_))
        && !state.smart_lexicon_v3_flags.read
    {
        return Err(v3_storage_unavailable());
    }
    apply_publication_legacy_bridge_read_flag(
        &mut response.publication,
        state.smart_lexicon_v3_flags.legacy_bridge_read,
    );
    apply_publication_sentence_association_flag(
        &mut response.publication,
        sentence_association_enabled(state.smart_lexicon_v3_flags),
        sentence_target_discovery_enabled(state.smart_lexicon_v3_flags),
        draft_relation_prebinding_enabled(state.smart_lexicon_v3_flags),
    );
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/surface-match-snapshots/{snapshot_id}",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(SurfaceMatchSnapshotPathV2, SurfaceMatchSnapshotQueryV2),
    responses(
        (status = 200, description = "不可变且可按 schema_version 判别的 surface match snapshot 下一页", body = SurfaceMatchPageAny),
        (status = 400, description = "snapshot ID 或 cursor 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 409, description = "snapshot 绑定的 surface policy 已变化"),
        (status = 410, description = "snapshot 已过期或 expand 阶段尚未建立"),
        (status = 503, description = "snapshot 存储不可用")
    )
)]
pub async fn surface_match_snapshot_page(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<SurfaceMatchSnapshotPathV2>,
    ApiQuery(query): ApiQuery<SurfaceMatchSnapshotQueryV2>,
) -> Result<(StatusCode, Json<SurfaceMatchPageAny>), AppError> {
    require_active_admin(&state, &auth).await?;
    let snapshots = crate::lexicon::surface_snapshot::SurfaceSnapshotStore::with_policy_prefix(
        state.redis.clone(),
        state.surface_policy_prefix.clone(),
    );
    match snapshots
        .page(auth.subject, path.snapshot_id, &query.cursor)
        .await
    {
        Ok(page) => Ok((StatusCode::OK, Json(page))),
        Err(crate::lexicon::surface_snapshot::SurfaceSnapshotError::Expired) => {
            Err(AppError::gone(
                ErrorCode::SurfaceMatchSnapshotExpired,
                "surface match snapshot expired",
            ))
        }
        Err(crate::lexicon::surface_snapshot::SurfaceSnapshotError::InvalidCursor) => {
            Err(AppError::bad_request(
                ErrorCode::InvalidQuery,
                "snapshot cursor is invalid or reused",
            ))
        }
        Err(crate::lexicon::surface_snapshot::SurfaceSnapshotError::PolicyChanged(name)) => {
            let policy = crate::lexicon::surface_policy::SurfacePolicyStore::with_prefix(
                state.redis.clone(),
                state.surface_policy_prefix.clone(),
            )
            .policy(name)
            .await
            .map_err(surface_policy_store_unavailable)?;
            Err(AppError::conflict(
                ErrorCode::SurfacePolicyChanged,
                None,
                "surface policy changed",
            )
            .with_meta(ProblemMeta {
                current_policy_name: Some(policy.name),
                current_policy_epoch: Some(policy.epoch),
                ..ProblemMeta::default()
            }))
        }
        Err(error @ crate::lexicon::surface_snapshot::SurfaceSnapshotError::Pool(_))
        | Err(error @ crate::lexicon::surface_snapshot::SurfaceSnapshotError::Redis(_))
        | Err(error @ crate::lexicon::surface_snapshot::SurfaceSnapshotError::Json(_)) => {
            Err(AppError::unavailable_with_source(
                ErrorCode::ServiceUnavailable,
                "surface snapshot service unavailable",
                error,
            ))
        }
        Err(error) => Err(AppError::internal(error)),
    }
}

fn surface_policy_store_unavailable(
    error: crate::lexicon::surface_policy::SurfacePolicyStoreError,
) -> AppError {
    AppError::unavailable_with_source(
        ErrorCode::ServiceUnavailable,
        "surface policy service unavailable",
        error,
    )
}

// --- queries ---

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/entries",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(AdminWordListQuery),
    responses(
        (status = 200, description = "智能词库分页列表", body = AdminWordListResponse),
        (status = 400, description = "查询参数非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 422, description = "列表包含当前 reader 不支持的 schema_version"),
        (status = 500, description = "数据库查询失败")
    )
)]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<AdminWordListQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .list(query, state.smart_lexicon_v3_flags.read)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/entries/related-search",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(RelatedSearchQuery),
    responses(
        (status = 200, description = "搜索当前发布版本中的关联词目标", body = RelatedSearchResponse),
        (status = 400, description = "查询参数非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 422, description = "publication snapshot schema_version 不受当前 reader 支持"),
        (status = 500, description = "数据库查询或发布快照解析失败")
    )
)]
pub async fn related_search(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<RelatedSearchQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    if query.include_drafts == Some(true)
        && !draft_relation_prebinding_enabled(state.smart_lexicon_v3_flags)
    {
        return Err(v3_storage_unavailable());
    }
    let response = service(&state)
        .related_search(auth.subject, query, state.smart_lexicon_v3_flags.read)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/entries/stats",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "累计、今日和本月词条数", body = AdminWordStats),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 500, description = "数据库查询失败")
    )
)]
pub async fn stats(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .stats(state.smart_lexicon_v3_flags.read)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}
