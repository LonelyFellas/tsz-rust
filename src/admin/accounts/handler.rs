use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    admin::{
        AdminAuth,
        accounts::{
            AdminAccountAdminResponse, AdminAccountsRepository, AdminAccountsService,
            AdminListQueryParams, AdminUserListResponse, model::UserListQueryParams,
            service::AdminAccountsServiceError,
        },
        authorization::{require_active_admin, require_super_admin},
    },
    api::{ApiJson, ApiQuery, ListQuery, PaginatedResponse, PaginationQuery},
    error::{AppError, ErrorCode},
    otp::{model::Purpose, service::OtpServiceError},
    platform::{Phone, PhoneError},
    state::AppState,
    user::{
        display_name::generate_display_name,
        model::{DisplayName, DisplayNameError},
        repository::UserRepository,
    },
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
    pub admin: AdminAccountAdminResponse,
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
    ApiJson(req): ApiJson<CreateAdminRequest>,
) -> Result<impl IntoResponse, AppError> {
    let admin = require_super_admin(&state, &auth).await?;

    // 2) 验证合法phone
    let phone = Phone::parse(&req.phone).map_err(map_phone_error)?;

    // 3) 如何display_name 不为空 则验证display_name
    let display_name = match &req.display_name {
        Some(display_name) => DisplayName::parse(display_name)
            .map_err(map_display_name_error)?
            .into_string(),
        // 自动系统帮生成一个
        _ => generate_display_name(),
    };

    state
        .otp_service
        .verify(admin.phone.as_str(), Purpose::AdminCreate, &req.code)
        .await
        .map_err(map_otp_verify_error)?;

    let admin_accounts_service =
        AdminAccountsService::new(AdminAccountsRepository::new(state.pool.clone()), None);
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
        (status = 200, description = "管理员列表查询成功", body = PaginatedResponse<AdminAccountAdminResponse>),
        (status = 400, description = "角色、分页或筛选参数非法"),
        (status = 401, description = "缺少/无效/过期 token，管理员不存在或账号被锁定"),
        (status = 403, description = "账号已禁用、必须先改密或不是超级管理员"),
        (status = 500, description = "数据库查询失败"),
    )
)]
pub async fn list_admins(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(filters): ApiQuery<AdminListQueryParams>,
    ApiQuery(pagination): ApiQuery<PaginationQuery>,
) -> Result<impl IntoResponse, AppError> {
    let query = ListQuery {
        filters,
        pagination,
    };

    require_super_admin(&state, &auth).await?;

    let service = AdminAccountsService::new(AdminAccountsRepository::new(state.pool.clone()), None);

    let response = service.admin_list(query).await.map_err(map_list_error)?;

    Ok((StatusCode::OK, Json(response)))
}

/// GET /api/v1/admin/users
/// 查询 C 端用户列表。
#[utoipa::path(
    get,
    path = "/api/v1/admin/users",
    tag = "admin-users",
    security(("bearer_auth" = [])),
    params(UserListQueryParams, PaginationQuery),
    responses(
        (status = 200, description = "用户列表查询成功", body = AdminUserListResponse),
        (status = 400, description = "角色、时间、分页或筛选参数非法"),
        (status = 401, description = "缺少/无效/过期 token，或管理员不存在"),
        (status = 403, description = "管理员账号已禁用或必须先改密"),
        (status = 500, description = "数据库查询失败"),
    )
)]
pub async fn list_users(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(filters): ApiQuery<UserListQueryParams>,
    ApiQuery(pagination): ApiQuery<PaginationQuery>,
) -> Result<impl IntoResponse, AppError> {
    let query = ListQuery {
        filters,
        pagination,
    };

    require_active_admin(&state, &auth).await?;

    let service = AdminAccountsService::new(
        AdminAccountsRepository::new(state.pool.clone()),
        Some(UserRepository::new(state.pool.clone())),
    );
    let response = service
        .user_list(query)
        .await
        .map_err(map_user_list_error)?;
    Ok((StatusCode::OK, Json(response)))
}

/// POST /api/v1/admin/admins/create-code
/// 向当前超级管理员的数据库手机号发送创建管理员确认码。
#[utoipa::path(
    post,
    path = "/api/v1/admin/admins/create-code",
    tag = "admin-accounts",
    security(("bearer_auth" = [])),
    responses(
        (status = 202, description = "创建管理员验证码已发送至当前超级管理员手机号"),
        (status = 401, description = "缺少/无效/过期 token，管理员不存在或账号被锁定"),
        (status = 403, description = "账号已禁用、必须先改密或不是超级管理员"),
        (status = 429, description = "验证码请求过于频繁"),
        (status = 503, description = "验证码存储或短信服务不可用"),
    )
)]
pub async fn request_create_admin_code(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<StatusCode, AppError> {
    let admin = require_super_admin(&state, &auth).await?;

    state
        .otp_service
        .request(&admin.phone, Purpose::AdminCreate)
        .await
        .map_err(map_otp_request_error)?;

    Ok(StatusCode::ACCEPTED)
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

fn map_otp_verify_error(error: OtpServiceError) -> AppError {
    match error {
        OtpServiceError::InvalidCode => {
            AppError::validation(ErrorCode::InvalidOtpCode, "code", "invalid code")
        }
        error @ OtpServiceError::Store(_) => {
            AppError::unavailable_with_source(ErrorCode::OtpUnavailable, "OTP unavailable", error)
        }
        OtpServiceError::Send(_) | OtpServiceError::RateLimited => {
            AppError::internal(anyhow::anyhow!("OTP verify returned an unexpected error"))
        }
    }
}

fn map_otp_request_error(error: OtpServiceError) -> AppError {
    match error {
        // 冷却期或每日发送次数超限
        OtpServiceError::RateLimited => {
            AppError::rate_limited(ErrorCode::OtpRateLimited, "too many requests")
        }

        // Redis 或短信服务不可用
        error @ (OtpServiceError::Store(_) | OtpServiceError::Send(_)) => {
            AppError::unavailable_with_source(ErrorCode::OtpUnavailable, "OTP unavailable", error)
        }

        // request() 不会产生 InvalidCode
        OtpServiceError::InvalidCode => AppError::internal(anyhow::anyhow!(
            "OTP request unexpectedly returned InvalidCode"
        )),
    }
}

fn map_list_error(error: AdminAccountsServiceError) -> AppError {
    match error {
        AdminAccountsServiceError::InvalidQuery(message) => {
            AppError::bad_request(ErrorCode::InvalidQuery, message)
        }
        other => AppError::internal(other),
    }
}

fn map_user_list_error(error: AdminAccountsServiceError) -> AppError {
    map_list_error(error)
}

fn map_provision_err(err: AdminAccountsServiceError) -> AppError {
    match err {
        AdminAccountsServiceError::AlreadyExists => AppError::conflict(
            ErrorCode::PhoneAlreadyRegistered,
            Some("phone"),
            "phone already registered",
        ),
        other => AppError::internal(other),
    }
}

fn map_phone_error(error: PhoneError) -> AppError {
    let message = match error {
        PhoneError::Empty => "phone is missing",
        PhoneError::Invalid => "invalid phone",
    };
    AppError::validation(ErrorCode::InvalidPhone, "phone", message)
}

fn map_display_name_error(error: DisplayNameError) -> AppError {
    AppError::validation(
        ErrorCode::InvalidDisplayName,
        "display_name",
        error.to_string(),
    )
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
