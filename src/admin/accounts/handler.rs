use axum::{
    Json,
    extract::{Query, State, rejection::QueryRejection},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    admin::{
        Admin, AdminRepository, AdminRepositoryError,
        accounts::{
            AdminAccountResponse, AdminAccountsRepository, AdminAccountsService,
            AdminListQueryParams, service::AdminAccountsServiceError,
        },
        extract::AdminAuth,
    },
    api::{ListQuery, PaginatedResponse, PaginationQuery},
    error::AppError,
    otp::{model::Purpose, service::OtpServiceError},
    platform::Phone,
    state::AppState,
    user::{display_name::generate_display_name, model::DisplayName},
};

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateAdminRequest {
    /// 待创建管理员的手机号。
    #[schema(example = "13800138000")]
    phone: String,
    /// 管理员昵称；不传时由系统自动生成。
    #[schema(example = "运营管理员")]
    display_name: Option<String>,
    /// `admin_create` 用途的短信验证码。
    #[schema(example = "123456")]
    code: String,
}

#[derive(Serialize, ToSchema)]
pub struct CreateAdminResponse {
    /// 创建完成的管理员公开信息。
    pub admin: AdminAccountResponse,
    /// 仅在本次响应中返回的临时密码。
    #[schema(example = "g7MpQ2xV9rKe4sY8uW3n")]
    pub temporary_password: String,
}

/// POST /api/v1/admin/admins
/// 超级管理员生成普通管理员
#[utoipa::path(
    post,
    path = "/api/v1/admin/admins",
    tag = "admin-accounts",
    security(("bearer_auth" = [])),
    request_body = CreateAdminRequest,
    responses(
        (status = 201, description = "普通管理员创建成功，返回公开信息和一次性临时密码", body = CreateAdminResponse),
        (status = 400, description = "手机号、昵称或短信验证码非法"),
        (status = 401, description = "缺少/无效/过期 token，管理员不存在或账号被锁定"),
        (status = 403, description = "账号已禁用、必须先改密或不是超级管理员"),
        (status = 409, description = "手机号已被其他管理员使用"),
        (status = 500, description = "数据库、密码生成或密码哈希失败"),
        (status = 503, description = "管理员账号服务不可用"),
    )
)]
pub async fn create_admin(
    State(state): State<AppState>,
    auth: AdminAuth,
    Json(req): Json<CreateAdminRequest>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_super_admin(&state, &auth).await?;

    // 2) 验证合法phone
    let phone =
        Phone::parse(&req.phone).map_err(|_| AppError::BadRequest("当前手机号码非法".into()))?;

    // 3) 如何display_name 不为空 则验证display_name
    let display_name = match &req.display_name {
        Some(display_name) => DisplayName::parse(display_name)
            .map_err(|_| AppError::BadRequest("当前用户名非法".into()))?
            .into_string(),
        // 自动系统帮生成一个
        _ => generate_display_name(),
    };

    state
        .otp_service
        .verify(phone.as_str(), Purpose::AdminCreate, &req.code)
        .await
        .map_err(map_otp_error)?;

    let admin_accounts_service =
        AdminAccountsService::new(AdminAccountsRepository::new(state.pool.clone()));
    let (new_admin, temporary_pwd) = admin_accounts_service
        .provision(admin.id, &admin.display_name, phone.as_str(), &display_name)
        .await
        .map_err(map_provision_err)?;

    Ok((
        StatusCode::CREATED,
        Json(CreateAdminResponse {
            admin: new_admin,
            temporary_password: temporary_pwd,
        }),
    ))
}

/// GET /api/v1/admin/admins
/// 查询管理员列表
#[utoipa::path(
    get,
    path = "/api/v1/admin/admins",
    tag = "admin-accounts",
    security(("bearer_auth" = [])),
    params(AdminListQueryParams, PaginationQuery),
    responses(
        (status = 200, description = "管理员列表查询成功", body = PaginatedResponse<AdminAccountResponse>),
        (status = 400, description = "角色、分页或筛选参数非法"),
        (status = 401, description = "缺少/无效/过期 token，管理员不存在或账号被锁定"),
        (status = 403, description = "账号已禁用、必须先改密或不是超级管理员"),
        (status = 500, description = "数据库查询失败"),
    )
)]
pub async fn list_admins(
    State(state): State<AppState>,
    auth: AdminAuth,
    filters: Result<Query<AdminListQueryParams>, QueryRejection>,
    pagination: Result<Query<PaginationQuery>, QueryRejection>,
) -> Result<impl IntoResponse, AppError> {
    let Query(filters) = filters.map_err(|error| AppError::BadRequest(error.to_string()))?;

    let Query(pagination) = pagination.map_err(|error| AppError::BadRequest(error.to_string()))?;

    let query = ListQuery {
        filters,
        pagination,
    };

    require_super_admin(&state, &auth).await?;

    let service = AdminAccountsService::new(AdminAccountsRepository::new(state.pool.clone()));

    let response = service.list(query).await.map_err(map_list_error)?;

    Ok(Json(response))
}

/// PATCH /api/v1/admin/admins/{id}/status
/// 启用/禁用普通管理
pub async fn set_admin_status() -> Result<impl IntoResponse, AppError> {
    Ok(())
}

/// POST /api/v1/admin/admins/{admin_id}/reset_password
/// 超级管理员对普通管理员密码重置
pub async fn reset_admin_password() -> Result<impl IntoResponse, AppError> {
    Ok(())
}

async fn require_super_admin(state: &AppState, auth: &AdminAuth) -> Result<Admin, AppError> {
    let admin = AdminRepository::new(state.pool.clone())
        .get_by_id(&auth.subject)
        .await
        .map_err(map_admin_error)?;

    if !admin.is_active() {
        return Err(AppError::Forbidden);
    }

    if admin.must_change_password {
        return Err(AppError::ForbiddenCode {
            message: "password change required".into(),
            code: "must_change_password".into(),
        });
    }

    if !admin.is_super_admin() {
        return Err(AppError::Forbidden);
    }

    if admin.is_locked(Utc::now()) {
        return Err(AppError::Unauthenticated("admin was locked".into()));
    }

    Ok(admin)
}

fn map_admin_error(err: AdminRepositoryError) -> AppError {
    match err {
        AdminRepositoryError::NotFound => AppError::Unauthenticated("admin not found".into()),
        _ => AppError::Internal(err.into()),
    }
}

fn map_otp_error(err: OtpServiceError) -> AppError {
    match err {
        OtpServiceError::InvalidCode => AppError::BadRequest("当前验证码非法".into()),
        e => AppError::Internal(e.into()),
    }
}

fn map_list_error(error: AdminAccountsServiceError) -> AppError {
    match error {
        AdminAccountsServiceError::InvalidQuery(message) => AppError::BadRequest(message),
        other => AppError::Internal(other.into()),
    }
}

fn map_provision_err(err: AdminAccountsServiceError) -> AppError {
    match err {
        AdminAccountsServiceError::AlreadyExists => {
            AppError::Conflict("phone already registered".into())
        }
        other => AppError::Internal(other.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_phone_maps_to_http_409() {
        let response = map_provision_err(AdminAccountsServiceError::AlreadyExists).into_response();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
}
