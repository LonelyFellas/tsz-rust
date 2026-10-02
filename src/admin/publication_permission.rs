use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    admin::{AdminAuth, accounts::AdminIdPath, authorization::require_super_admin},
    api::{ApiJson, ApiPath},
    error::AppError,
    state::AppState,
};

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconPublicationPermission {
    pub can_publish_lexicon: bool,
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/admins/{admin_id}/lexicon-publication-permission",
    tag = "admin-accounts",
    security(("bearer_auth" = [])),
    params(AdminIdPath),
    request_body = LexiconPublicationPermission,
    responses(
        (status = 409, description = "Upgrade required: use versioned permission changes"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "必须由超管操作"),
        (status = 422, description = "请求体非法")
    )
)]
pub async fn update(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(_path): ApiPath<AdminIdPath>,
    ApiJson(_input): ApiJson<LexiconPublicationPermission>,
) -> Result<Json<LexiconPublicationPermission>, AppError> {
    require_super_admin(&state, &auth).await?;
    Err(crate::admin::permissions::service::conflict(
        "client upgrade required: use /admin/permission-changes",
    ))
}
