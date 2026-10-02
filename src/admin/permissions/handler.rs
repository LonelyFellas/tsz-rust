use axum::{
    Extension, Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};

use super::{catalog, model::*, service};
use crate::{
    admin::{AdminAuth, accounts::AdminIdPath, authorization::require_super_admin},
    api::{ApiJson, ApiPath, ApiQuery},
    error::AppError,
    request_id::RequestId,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/permissions", get(list))
        .route("/admins/{admin_id}/permissions", get(admin_permissions))
        .route("/permissions/{permission_key}/admins", get(granted_admins))
        .route("/permission-changes/preview", post(preview))
        .route("/permission-changes", post(apply))
        .route("/permission-tags", get(list_tags).post(create_tag))
        .route(
            "/permission-tags/{tag_id}",
            axum::routing::patch(rename_tag).delete(delete_tag),
        )
        .route("/permission-tag-changes", post(change_tags))
        .route("/permission-audits", get(audits))
}

#[utoipa::path(get, path = "/api/v1/admin/permissions", tag = "admin-permissions", security(("bearer_auth" = [])), responses((status = 200, body = PermissionCatalog), (status = 403, description = "Super admin required")))]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Json<PermissionCatalog>, AppError> {
    require_super_admin(&state, &auth).await?;
    Ok(Json(PermissionCatalog {
        catalog_version: catalog::catalog_version(),
        permissions: catalog::CATALOG.to_vec(),
        tags: service::tags(&state.pool).await?,
    }))
}

#[utoipa::path(get, path = "/api/v1/admin/admins/{admin_id}/permissions", tag = "admin-permissions", security(("bearer_auth" = [])), params(AdminIdPath), responses((status = 200, body = AdminPermissions), (status = 403, description = "Super admin required"), (status = 404, description = "Admin not found")))]
pub async fn admin_permissions(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<AdminIdPath>,
) -> Result<Json<AdminPermissions>, AppError> {
    require_super_admin(&state, &auth).await?;
    Ok(Json(
        service::snapshot(&state.pool, auth.subject, path.admin_id).await?,
    ))
}

#[utoipa::path(post, path = "/api/v1/admin/permission-changes/preview", tag = "admin-permissions", security(("bearer_auth" = [])), request_body = PreviewRequest, responses((status = 200, body = PreviewResponse), (status = 403, description = "Super admin required"), (status = 409, description = "Version conflict"), (status = 422, description = "Invalid changes")))]
pub async fn preview(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<PreviewRequest>,
) -> Result<Json<PreviewResponse>, AppError> {
    require_super_admin(&state, &auth).await?;
    Ok(Json(
        service::preview(&state.pool, auth.subject, input).await?,
    ))
}

#[utoipa::path(post, path = "/api/v1/admin/permission-changes", tag = "admin-permissions", security(("bearer_auth" = [])), request_body = ChangeRequest, responses((status = 200, body = ChangeResponse), (status = 403, description = "Super admin required"), (status = 409, description = "Version conflict"), (status = 422, description = "Invalid changes")))]
pub async fn apply(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    ApiJson(input): ApiJson<ChangeRequest>,
) -> Result<Json<ChangeResponse>, AppError> {
    require_super_admin(&state, &auth).await?;
    Ok(Json(
        service::apply(&state.pool, auth.subject, request_id.as_uuid(), input).await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/admin/permission-tags", tag = "admin-permissions", security(("bearer_auth" = [])), responses((status = 200, body = Vec<PermissionTag>), (status = 403, description = "Super admin required")))]
pub async fn list_tags(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Json<Vec<PermissionTag>>, AppError> {
    require_super_admin(&state, &auth).await?;
    Ok(Json(service::tags(&state.pool).await?))
}

#[utoipa::path(post, path = "/api/v1/admin/permission-tags", tag = "admin-permissions", security(("bearer_auth" = [])), request_body = CreateTagRequest, responses((status = 201, body = PermissionTag), (status = 403, description = "Super admin required"), (status = 409, description = "Name conflict"), (status = 422, description = "Invalid name")))]
pub async fn create_tag(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    ApiJson(input): ApiJson<CreateTagRequest>,
) -> Result<(StatusCode, Json<PermissionTag>), AppError> {
    require_super_admin(&state, &auth).await?;
    Ok((
        StatusCode::CREATED,
        Json(
            service::create_tag(&state.pool, auth.subject, request_id.as_uuid(), &input.name)
                .await?,
        ),
    ))
}

#[utoipa::path(patch, path = "/api/v1/admin/permission-tags/{tag_id}", tag = "admin-permissions", security(("bearer_auth" = [])), params(TagIdPath), request_body = UpdateTagRequest, responses((status = 204, description = "Renamed"), (status = 403, description = "Super admin required"), (status = 409, description = "Version or name conflict")))]
pub async fn rename_tag(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    ApiPath(path): ApiPath<TagIdPath>,
    ApiJson(input): ApiJson<UpdateTagRequest>,
) -> Result<StatusCode, AppError> {
    require_super_admin(&state, &auth).await?;
    service::update_tag(
        &state.pool,
        auth.subject,
        request_id.as_uuid(),
        path.tag_id,
        input.expected_version,
        Some(&input.name),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/v1/admin/permission-tags/{tag_id}", tag = "admin-permissions", security(("bearer_auth" = [])), params(TagIdPath, DeleteTagQuery), responses((status = 204, description = "Deleted without changing grants"), (status = 403, description = "Super admin required"), (status = 409, description = "Version conflict")))]
pub async fn delete_tag(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    ApiPath(path): ApiPath<TagIdPath>,
    ApiQuery(query): ApiQuery<DeleteTagQuery>,
) -> Result<StatusCode, AppError> {
    require_super_admin(&state, &auth).await?;
    service::update_tag(
        &state.pool,
        auth.subject,
        request_id.as_uuid(),
        path.tag_id,
        query.expected_version,
        None,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/admin/permission-tag-changes", tag = "admin-permissions", security(("bearer_auth" = [])), request_body = TagChangeRequest, responses((status = 200, body = Vec<PermissionTag>), (status = 403, description = "Super admin required"), (status = 409, description = "Version conflict"), (status = 422, description = "Invalid changes")))]
pub async fn change_tags(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<RequestId>,
    ApiJson(input): ApiJson<TagChangeRequest>,
) -> Result<Json<Vec<PermissionTag>>, AppError> {
    require_super_admin(&state, &auth).await?;
    Ok(Json(
        service::change_tags(&state.pool, auth.subject, request_id.as_uuid(), input).await?,
    ))
}

fn pagination(page: Option<i64>, size: Option<i64>) -> Result<(i64, i64), AppError> {
    let page = page.unwrap_or(1);
    let size = size.unwrap_or(20);
    if !(1..=1_000_000).contains(&page) || !(1..=100).contains(&size) {
        return Err(catalog::invalid("invalid pagination"));
    }
    Ok((page, size))
}

#[utoipa::path(get, path = "/api/v1/admin/permissions/{permission_key}/admins", tag = "admin-permissions", security(("bearer_auth" = [])), params(PermissionKeyPath, PermissionListQuery), responses((status = 200, body = GrantedAdmins), (status = 403, description = "Super admin required"), (status = 422, description = "Unknown permission")))]
pub async fn granted_admins(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<PermissionKeyPath>,
    ApiQuery(query): ApiQuery<PermissionListQuery>,
) -> Result<Json<GrantedAdmins>, AppError> {
    require_super_admin(&state, &auth).await?;
    if catalog::definition(&path.permission_key).is_none() {
        return Err(catalog::invalid("unknown permission"));
    }
    let (page, page_size) = pagination(query.page, query.page_size)?;
    let total = sqlx::query_scalar("SELECT count(*) FROM admin_permission_grants g JOIN admins a ON a.id = g.admin_id WHERE g.permission_key = $1 AND a.role = 'admin'")
        .bind(&path.permission_key).fetch_one(&state.pool).await.map_err(AppError::internal)?;
    let rows = sqlx::query_as::<_, (Uuid, String, i64)>("SELECT a.id, a.display_name, a.permission_version FROM admin_permission_grants g JOIN admins a ON a.id = g.admin_id WHERE g.permission_key = $1 AND a.role = 'admin' ORDER BY a.id LIMIT $2 OFFSET $3")
        .bind(&path.permission_key).bind(page_size).bind((page - 1) * page_size).fetch_all(&state.pool).await.map_err(AppError::internal)?;
    Ok(Json(GrantedAdmins {
        items: rows
            .into_iter()
            .map(
                |(admin_id, display_name, permission_version)| GrantedAdmin {
                    admin_id,
                    display_name,
                    permission_version,
                },
            )
            .collect(),
        total,
        page,
        page_size,
        super_admins_are_implicit: true,
    }))
}

use uuid::Uuid;

#[utoipa::path(get, path = "/api/v1/admin/permission-audits", tag = "admin-permissions", security(("bearer_auth" = [])), params(AuditQuery), responses((status = 200, body = PermissionAudits), (status = 403, description = "Super admin required")))]
pub async fn audits(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<AuditQuery>,
) -> Result<Json<PermissionAudits>, AppError> {
    require_super_admin(&state, &auth).await?;
    let (page, page_size) = pagination(query.page, query.page_size)?;
    const FILTER: &str = " WHERE action IN ('admin.permissions.change', 'admin.permission_tags.create', 'admin.permission_tags.rename', 'admin.permission_tags.delete', 'admin.permission_tags.items') AND ($1::uuid IS NULL OR resource_id = $1 OR actor_admin_id = $1) AND ($2::text IS NULL OR metadata->'grant' ? $2 OR metadata->'revoke' ? $2 OR metadata->'before' ? $2 OR metadata->'after' ? $2 OR metadata->'before'->'permissions' ? $2) AND ($3::timestamptz IS NULL OR occurred_at >= $3) AND ($4::timestamptz IS NULL OR occurred_at <= $4)";
    let mut count_sql =
        sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT count(*) FROM audit.admin_actions");
    count_sql.push(FILTER);
    let total = count_sql
        .build_query_scalar::<i64>()
        .bind(query.admin_id)
        .bind(&query.permission_key)
        .bind(query.since)
        .bind(query.until)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::internal)?;
    let mut items_sql = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "SELECT id, actor_admin_id, action, resource_type, resource_id, request_id, metadata, occurred_at FROM audit.admin_actions",
    );
    items_sql
        .push(FILTER)
        .push(" ORDER BY occurred_at DESC, id DESC LIMIT $5 OFFSET $6");
    let items = items_sql
        .build_query_as::<PermissionAudit>()
        .bind(query.admin_id)
        .bind(&query.permission_key)
        .bind(query.since)
        .bind(query.until)
        .bind(page_size)
        .bind((page - 1) * page_size)
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::internal)?;
    Ok(Json(PermissionAudits {
        items,
        total,
        page,
        page_size,
    }))
}
