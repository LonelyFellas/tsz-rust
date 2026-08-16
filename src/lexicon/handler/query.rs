use super::*;

// --- entry query ---

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/entries/{id}",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(EntryPath),
    responses(
        (status = 200, description = "V2 canonical 词条草稿", body = AdminWordV2Envelope),
        (status = 400, description = "词条 ID 非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "词条不存在"),
        (status = 500, description = "数据库查询失败")
    )
)]
pub async fn get(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<EntryPath>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state).get(path.id).await.map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/surface-match-snapshots/{snapshot_id}",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(SurfaceMatchSnapshotPathV2, SurfaceMatchSnapshotQueryV2),
    responses(
        (status = 200, description = "不可变 surface match snapshot 的下一页", body = SurfaceMatchPageV2),
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
) -> Result<(StatusCode, Json<SurfaceMatchPageV2>), AppError> {
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
        (status = 500, description = "数据库查询失败")
    )
)]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<AdminWordListQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state).list(query).await.map_err(map_error)?;
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
        (status = 500, description = "数据库查询或发布快照解析失败")
    )
)]
pub async fn related_search(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<RelatedSearchQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .related_search(query)
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
    let response = service(&state).stats().await.map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}
