use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    admin::{AdminRole, AdminStatus},
    api::{ListQuery, PaginatedResponse},
    user::model::{UserRole, UserStatus},
};

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminCreatorResponse {
    pub id: Uuid,
    pub display_name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminAccountAdminResponse {
    pub id: Uuid,
    pub phone: String,
    pub display_name: String,
    pub role: AdminRole,
    pub created_by: Option<AdminCreatorResponse>,
    pub status: AdminStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminAccountUserResponse {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub phone: Option<String>,
    #[schema(nullable = false)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub display_name: String,
    pub avatar_url: String,
    pub roles: Vec<UserRole>,
    pub status: UserStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminUserListResponse {
    pub items: Vec<AdminAccountUserResponse>,
    pub page: crate::api::PaginationMeta,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AdminListQueryParams {
    /// 管理员角色筛选
    pub role: Option<AdminRole>,

    /// 手机号
    pub phone: Option<String>,

    /// 昵称
    pub display_name: Option<String>,
}

/// `/admins/{admin_id}/...` 的路径参数。
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
pub struct AdminIdPath {
    pub admin_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UserListQueryParams {
    /// 按用户持有的角色筛选
    pub role: Option<UserRole>,
    /// 手机号、邮箱或昵称的字面子串匹配
    pub q: Option<String>,
    /// 注册时间下界（含，RFC 3339）
    pub registered_from: Option<DateTime<Utc>>,
    /// 注册时间上界（不含，RFC 3339）
    pub registered_to: Option<DateTime<Utc>>,
}

pub type AdminListResponse = PaginatedResponse<AdminAccountAdminResponse>;
pub type UserListResponse = AdminUserListResponse;
pub type AdminListQuery = ListQuery<AdminListQueryParams>;
pub type UserListQuery = ListQuery<UserListQueryParams>;

#[derive(Debug)]
pub(crate) struct AdminAccountAdminListFilter {
    pub role: Option<AdminRole>,
    pub phone_pattern: Option<String>,
    pub display_name_pattern: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct AdminAccountRecord {
    pub id: Uuid,
    pub phone: String,
    pub display_name: String,
    pub role: AdminRole,
    pub status: AdminStatus,

    pub created_by_id: Option<Uuid>,
    pub created_by_display_name: Option<String>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
