use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    admin::{AdminRole, AdminStatus},
    api::{ListQuery, PaginatedResponse},
    user::model::{CefrLevel, EnglishVariant, UserRole},
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
    pub phone: Option<String>,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub student_role_cefr_level: CefrLevel,
    pub student_role_english_variant: EnglishVariant,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UserListQueryParams {
    /// 用户昵称
    pub display_name: Option<String>,
    /// 用户手机号
    pub phone: Option<String>,
    /// 用户邮箱
    pub email: Option<String>,
    /// 用户角色
    pub role: Option<UserRole>,
    /// 注册开始时间
    pub registration_start_time: DateTime<Utc>,
    /// 注册结束时间
    pub registration_end_time: DateTime<Utc>,
}

pub type AdminListResponse = PaginatedResponse<AdminAccountAdminResponse>;
pub type UserListResponse = PaginatedResponse<AdminAccountUserResponse>;
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
