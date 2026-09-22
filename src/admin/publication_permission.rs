use axum::{Extension, Json, extract::State};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    admin::{AdminAuth, accounts::AdminIdPath, authorization::require_super_admin},
    api::{ApiJson, ApiPath},
    error::{AppError, ErrorCode},
    state::AppState,
};

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconPublicationPermission {
    pub can_publish_lexicon: bool,
}

pub(crate) async fn effective(pool: &sqlx::PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT role = 'super_admin' OR can_publish_lexicon FROM admins WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
}

/// 与授权变更共用管理员行锁：撤权生效之后的新发布不得沿用旧权限。
/// 返回超管身份供同一事务的草稿归属校验使用。
pub(crate) async fn lock_publisher(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<bool>, sqlx::Error> {
    let row = sqlx::query_as::<_, (String, bool, String, bool)>(
        "SELECT role, can_publish_lexicon, status, must_change_password FROM admins WHERE id = $1 FOR SHARE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.and_then(|(role, allowed, status, must_change)| {
        (status == "active" && !must_change && (role == "super_admin" || allowed))
            .then_some(role == "super_admin")
    }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/admins/{admin_id}/lexicon-publication-permission",
    tag = "admin-accounts",
    security(("bearer_auth" = [])),
    params(AdminIdPath),
    request_body = LexiconPublicationPermission,
    responses(
        (status = 200, description = "词库发布授权已更新", body = LexiconPublicationPermission),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "必须由超管操作，且不能修改超管的固有权限"),
        (status = 404, description = "管理员不存在"),
        (status = 422, description = "请求体非法")
    )
)]
pub async fn update(
    State(state): State<AppState>,
    auth: AdminAuth,
    Extension(request_id): Extension<crate::request_id::RequestId>,
    ApiPath(path): ApiPath<AdminIdPath>,
    ApiJson(input): ApiJson<LexiconPublicationPermission>,
) -> Result<Json<LexiconPublicationPermission>, AppError> {
    let actor = require_super_admin(&state, &auth).await?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let target = sqlx::query_as::<_, (String, bool)>(
        "SELECT role, can_publish_lexicon FROM admins WHERE id = $1 FOR UPDATE",
    )
    .bind(path.admin_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("admin not found"))?;
    if target.0 == "super_admin" {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "super admin permission is intrinsic",
        ));
    }
    sqlx::query("UPDATE admins SET can_publish_lexicon = $2, updated_at = now() WHERE id = $1")
        .bind(path.admin_id)
        .bind(input.can_publish_lexicon)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("INSERT INTO audit.admin_actions (id, actor_admin_id, action, resource_type, resource_id, request_id, metadata) VALUES ($1, $2, 'admin.lexicon_publication_permission.update', 'admin', $3, $4, $5)")
        .bind(Uuid::now_v7()).bind(actor.id).bind(path.admin_id).bind(request_id.as_uuid())
        .bind(serde_json::json!({"before": target.1, "after": input.can_publish_lexicon}))
        .execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(input))
}
