use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;

use crate::{
    error::AppError,
    user::{
        model::{PasswordError, SubjectError, User},
        repository::UserRepository,
        service::{RegisterError, RegisterInput, UserService},
    },
};

/// DTO
#[derive(Deserialize, ToSchema)]
pub struct RegisterRequest {
    /// 手机号（与 email 二选一）
    #[schema(example = "13800138000")]
    phone: Option<String>,
    /// 邮箱（与 phone 二选一）
    #[schema(example = "student@example.com")]
    email: Option<String>,
    #[schema(example = "P@ssw0rd!")]
    password: String,
}

#[derive(Serialize, ToSchema)]
pub struct RegisterResponse {
    #[schema(example = "0198f2a1-3b4c-7d5e-8f90-1a2b3c4d5e6f")]
    pub user_id: String,
    #[schema(example = "同学1234")]
    pub display_name: String,
    #[schema(example = "student")]
    pub role: &'static str,
}

/// POST /api/v1/user/register
#[utoipa::path(
    post,
    path = "/api/v1/user/register",
    tag = "user",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "注册成功", body = RegisterResponse),
        (status = 400, description = "手机号/邮箱缺失或密码格式不合法"),
        (status = 409, description = "手机号或邮箱已被占用"),
    )
)]
pub async fn register(
    State(pool): State<PgPool>,
    Json(payload): Json<RegisterRequest>,
) -> Result<impl IntoResponse, AppError> {
    // 1) 现建 service
    let service = UserService::new(UserRepository::new(pool));

    // 2) 调用 service 的 register 方法
    let user = service
        .register(RegisterInput {
            phone: payload.phone,
            email: payload.email,
            password: payload.password,
        })
        .await
        .map_err(map_register_error)?; // 3) 领域错误 -> HTTP 错误

    // 4) 返回201响应

    Ok((StatusCode::CREATED, Json(to_response(user))))
}

fn to_response(user: User) -> RegisterResponse {
    let role = user.active_role().as_str();
    RegisterResponse {
        user_id: user.id.to_string(),
        display_name: user.display_name,
        role,
    }
}

fn map_register_error(err: RegisterError) -> AppError {
    match err {
        // 手机 / 邮箱 已被占用
        RegisterError::Register(SubjectError::UserAlreadyExists) => {
            AppError::Conflict("user already exists".into())
        }
        // 手机号 / 邮箱 格式为空
        RegisterError::Register(SubjectError::PhoneOrEmailMissing) => {
            AppError::BadRequest("phone or email is missing".into())
        }
        // 其余 SubjectError 错误
        RegisterError::Register(_) => AppError::BadRequest("unknown subject error".into()),
        // 密码格式为空
        RegisterError::Password(PasswordError::Empty) => {
            AppError::BadRequest("password is missing".into())
        }
        // 密码格式错误
        RegisterError::Password(_) => AppError::BadRequest("invalid password".into()),
        // 仓储/DB 错误 -> 500
        RegisterError::Repository(_) => AppError::internal(err),
    }
}
