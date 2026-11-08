use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    admin::{
        AdminDialectPreference, AdminRole, authorization::require_active_admin, extract::AdminAuth,
    },
    api::ApiJson,
    error::{AppError, ErrorCode},
    i18n::{InterfaceLocaleV1, InterfaceSurface, present_nullable},
    state::AppState,
};

/// GET /profile 的响应：当前身份、生效权限与个人偏好。
#[derive(Serialize, ToSchema)]
pub struct AdminProfileResponse {
    pub can_publish_lexicon: bool,
    pub id: Uuid,
    pub phone: String,
    pub display_name: String,
    pub role: AdminRole,
    pub permissions: Vec<String>,
    pub permission_version: i64,
    pub catalog_version: String,
    /// 个人偏好；字段恒在，从未设置过的管理员返回默认值。
    pub preferences: AdminPreferences,
}

/// 管理员个人偏好；英语方言与界面语言分别保存。
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct AdminPreferences {
    /// 英语方言偏好；**默认值只由后端持有**，前端不再保留第二处默认。
    pub dialect: AdminDialectPreference,
    #[schema(required = true)]
    pub interface_locale: Option<InterfaceLocaleV1>,
}

/// PATCH /profile/preferences 的请求体。只带要改的偏好，改的恒是自己的。
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateAdminPreferencesRequest {
    /// 仅提供时修改；不接受 null，非法值返回 422 invalid_request_body。
    #[serde(default, deserialize_with = "optional_dialect")]
    #[schema(nullable = false)]
    pub dialect: Option<AdminDialectPreference>,
    #[serde(default, deserialize_with = "present_nullable")]
    #[schema(value_type = Option<InterfaceLocaleV1>)]
    pub interface_locale: Option<Option<InterfaceLocaleV1>>,
}

fn optional_dialect<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<AdminDialectPreference>, D::Error> {
    match String::deserialize(deserializer)?.as_str() {
        "uk" => Ok(Some(AdminDialectPreference::Uk)),
        "us" => Ok(Some(AdminDialectPreference::Us)),
        _ => Err(serde::de::Error::custom("invalid dialect")),
    }
}

/// PATCH /profile/preferences 的响应：落库后的完整偏好。
#[derive(Serialize, ToSchema)]
pub struct UpdateAdminPreferencesResponse {
    pub preferences: AdminPreferences,
}

/// GET /api/v1/admin/profile
///
/// 登录管理员自身档案（/me 型身份探针）：前端用它确认会话有效、渲染顶栏与侧栏菜单。
/// 刻意**不查** locked_until——锁定语义只挡新登录/refresh 轮换（防爆破），不打断
/// 已认证的短命 access token，否则错码轰炸可把在线管理员打下线（DoS）。
#[utoipa::path(
    get,
    path = "/api/v1/admin/profile",
    tag = "admin",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "登录管理员完整档案", body = AdminProfileResponse),
        (status = 401, description = "缺/无效/过期 token，或账号已不存在（视为过期会话）"),
        (status = 403, description = "Account disabled (code=account_disabled), or password change required (code=must_change_password)"),
    )
)]
pub async fn admin_profile(
    State(state): State<AppState>,
    admin: AdminAuth,
) -> Result<impl IntoResponse, AppError> {
    let authorization = crate::admin::permissions::load(&state, &admin).await?;
    let admin = require_active_admin(&state, &admin).await?;
    Ok((
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(AdminProfileResponse {
            can_publish_lexicon: crate::admin::permissions::legacy_publication_allowed(
                &authorization,
            ),
            id: admin.id,
            phone: admin.phone,
            display_name: admin.display_name,
            role: admin.role,
            permissions: authorization.permissions.into_iter().collect(),
            permission_version: authorization.permission_version,
            catalog_version: crate::admin::permissions::catalog::catalog_version(),
            preferences: sqlx::query_as::<_, AdminPreferences>(
                "SELECT dialect_preference AS dialect,interface_locale FROM admins WHERE id=$1",
            )
            .bind(admin.id)
            .fetch_one(&state.pool)
            .await
            .map_err(AppError::internal)?,
        }),
    ))
}

/// PATCH /api/v1/admin/profile/preferences
///
/// 管理员改**自己的**个人偏好：目标恒为 token subject，请求体里没有管理员 ID，
/// 因此不存在改他人偏好的入口，也就不需要额外的角色判定。
/// 守卫沿用 profile 那一组（disabled / must_change_password 均 403）。
#[utoipa::path(
    patch,
    path = "/api/v1/admin/profile/preferences",
    tag = "admin",
    security(("bearer_auth" = [])),
    request_body = UpdateAdminPreferencesRequest,
    responses(
        (status = 200, description = "偏好已更新，返回落库后的完整偏好", body = UpdateAdminPreferencesResponse),
        (status = 401, description = "缺/无效/过期 token，或账号已不存在（视为过期会话）"),
        (status = 403, description = "Account disabled (code=account_disabled), or password change required (code=must_change_password)"),
        (status = 422, description = "请求体缺字段或 dialect 不在枚举内（code=invalid_request_body）"),
    )
)]
pub async fn update_admin_preferences(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(request): ApiJson<UpdateAdminPreferencesRequest>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    if request.dialect.is_none() && request.interface_locale.is_none() {
        return Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "at least one preference is required",
        ));
    }
    state
        .interface_languages
        .require_supported(InterfaceSurface::Admin, request.interface_locale.flatten())?;
    let preferences = sqlx::query_as::<_, AdminPreferences>(
        "UPDATE admins SET dialect_preference=COALESCE($3,dialect_preference),interface_locale=CASE WHEN $4 THEN $5 ELSE interface_locale END,updated_at=NOW() WHERE id=$1 AND security_version=$2 AND status='active' AND NOT must_change_password RETURNING dialect_preference AS dialect,interface_locale")
        .bind(admin.id).bind(auth.security_version).bind(request.dialect).bind(request.interface_locale.is_some()).bind(request.interface_locale.flatten())
        .fetch_optional(&state.pool).await.map_err(AppError::internal)?
        .ok_or_else(|| AppError::unauthorized(ErrorCode::InvalidToken,"invalid token"))?;

    Ok((
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(UpdateAdminPreferencesResponse { preferences }),
    ))
}
