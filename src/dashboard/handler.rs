use axum::{Json, extract::State};

use crate::{
    admin::{AdminAuth, authorization::require_active_admin},
    error::AppError,
    state::AppState,
};

use super::{dto::AdminDashboardOverview, repository};

#[utoipa::path(
    get,
    path = "/api/v1/admin/dashboard/overview",
    tag = "admin-dashboard",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "管理端首页看板聚合数据", body = AdminDashboardOverview),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 500, description = "数据库查询失败")
    )
)]
pub async fn overview(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Json<AdminDashboardOverview>, AppError> {
    require_active_admin(&state, &auth).await?;
    let overview = repository::load(&state.pool, state.smart_lexicon_v3_flags.read)
        .await
        .map_err(AppError::internal)?;
    Ok(Json(overview))
}
