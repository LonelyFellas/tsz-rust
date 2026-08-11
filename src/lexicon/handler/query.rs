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
