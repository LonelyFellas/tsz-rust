use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::{
    error::AppError,
    user::{
        model::{PasswordError, SubjectError, User, UserRole},
        repository::UserRepository,
        service::{RegisterError, RegisterInput, UserService},
    },
};

/// DTO
#[derive(Deserialize)]
pub struct RegisterRequest {
    phone: Option<String>,
    email: Option<String>,
    password: String,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub user_id: String,
    pub display_name: String,
    pub role: &'static str,
}

/// POST /api/v1/user/register
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
    RegisterResponse {
        user_id: user.id.to_string(),
        display_name: user.display_name,
        role: match user.last_active_role {
            Some(UserRole::Teacher) => "teacher",
            _ => "student",
        },
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
