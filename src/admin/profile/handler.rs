use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    admin::{
        AdminDialectPreference, AdminRepository, AdminRepositoryError, AdminRole,
        authorization::require_active_admin, extract::AdminAuth,
    },
    api::ApiJson,
    error::{AppError, ErrorCode},
    state::AppState,
};

/// 侧栏菜单权限 key 全量目录（顺序即侧栏顺序）。Q10 取消 RBAC 后全员全功能，
/// profile 恒返这份死数据，仅为保前端菜单渲染逻辑零改动。
pub const MENU_PERMISSIONS: [&str; 12] = [
    "users.access",
    "classes.access",
    "words.access",
    "customdict.access",
    "sentences.access",
    "wordlists.access",
    "customwordlist.access",
    "tasks.access",
    "reviews.access",
    "teacherapply.access",
    "comments.access",
    "coins.access",
];

/// GET /profile 的响应：login 的 4 字段概要 + 菜单权限目录 + 个人偏好。
#[derive(Serialize, ToSchema)]
pub struct AdminProfileResponse {
    pub id: Uuid,
    pub phone: String,
    pub display_name: String,
    pub role: AdminRole,
    /// 菜单权限 key 全量目录（恒为数组，Q10 死数据；顺序即侧栏顺序）
    pub permissions: Vec<&'static str>,
    /// 个人偏好；字段恒在，从未设置过的管理员返回默认值。
    pub preferences: AdminPreferences,
}

/// 管理员个人偏好。眼下只有方言一项，仍嵌一层对象：将来加第二项时
/// profile 响应的形状不用再变，前端也不必区分「顶层字段」与「偏好」。
#[derive(Serialize, ToSchema)]
pub struct AdminPreferences {
    /// 英语方言偏好；**默认值只由后端持有**，前端不再保留第二处默认。
    pub dialect: AdminDialectPreference,
}

/// PATCH /profile/preferences 的请求体。只带要改的偏好，改的恒是自己的。
#[derive(Deserialize, ToSchema)]
pub struct UpdateAdminPreferencesRequest {
    /// 目标方言偏好；枚举外的取值由 `ApiJson` 统一挡成 422 invalid_request_body。
    pub dialect: AdminDialectPreference,
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
    let admin = AdminRepository::new(state.pool.clone())
        .get_by_id(&admin.subject)
        .await
        .map_err(map_admin_error)?;

    if !admin.is_active() {
        return Err(AppError::forbidden(ErrorCode::AccountDisabled, "forbidden"));
    }

    // must_change 守卫（§7 守卫组）：目前 profile 是唯一守卫组端点，内联在此；
    // change-password/logout-all 落地后若守卫组扩员，再抽成 middleware/组合提取器。
    if admin.must_change_password {
        return Err(AppError::forbidden(
            ErrorCode::MustChangePassword,
            "password change required",
        ));
    }

    Ok((
        StatusCode::OK,
        Json(AdminProfileResponse {
            id: admin.id,
            phone: admin.phone,
            display_name: admin.display_name,
            role: admin.role,
            permissions: MENU_PERMISSIONS.to_vec(),
            preferences: AdminPreferences {
                dialect: admin.dialect_preference,
            },
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
    let dialect = AdminRepository::new(state.pool.clone())
        .set_dialect_preference(&admin.id, request.dialect)
        .await
        .map_err(map_admin_error)?;

    Ok((
        StatusCode::OK,
        Json(UpdateAdminPreferencesResponse {
            preferences: AdminPreferences { dialect },
        }),
    ))
}

fn map_admin_error(err: AdminRepositoryError) -> AppError {
    match err {
        AdminRepositoryError::NotFound => {
            AppError::unauthorized(ErrorCode::AdminNotFound, "admin not found")
        }
        _ => AppError::internal(err),
    }
}
