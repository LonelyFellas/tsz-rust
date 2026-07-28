use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    admin::{AdminRepository, AdminRepositoryError, AdminRole, extract::AdminAuth},
    error::AppError,
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

/// GET /profile 的响应：login 的 4 字段概要 + 菜单权限目录。
#[derive(Serialize, ToSchema)]
pub struct AdminProfileResponse {
    pub id: Uuid,
    pub phone: String,
    pub display_name: String,
    pub role: AdminRole,
    /// 菜单权限 key 全量目录（恒为数组，Q10 死数据；顺序即侧栏顺序）
    pub permissions: Vec<&'static str>,
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
        (status = 403, description = "账号已禁用（无 code）；或须先改密（code=must_change_password，前端据此跳改密页）"),
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
        return Err(AppError::Forbidden);
    }

    // must_change 守卫（§7 守卫组）：目前 profile 是唯一守卫组端点，内联在此；
    // change-password/logout-all 落地后若守卫组扩员，再抽成 middleware/组合提取器。
    if admin.must_change_password {
        return Err(AppError::ForbiddenCode {
            message: "password change required".into(),
            code: "must_change_password".into(),
        });
    }

    Ok((
        StatusCode::OK,
        Json(AdminProfileResponse {
            id: admin.id,
            phone: admin.phone,
            display_name: admin.display_name,
            role: admin.role,
            permissions: MENU_PERMISSIONS.to_vec(),
        }),
    ))
}

fn map_admin_error(err: AdminRepositoryError) -> AppError {
    match err {
        AdminRepositoryError::NotFound => AppError::Unauthenticated("admin not found".into()),
        _ => AppError::Internal(err.into()),
    }
}
