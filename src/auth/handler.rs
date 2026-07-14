use crate::{
    auth::extract::AuthUser,
    constant::TOKEN_SCHEMA,
    error::AppError,
    otp::{model::Purpose, service::OtpServiceError},
    session::{
        repository::RefreshTokenRepository,
        service::{SessionError, SessionService},
    },
    state::AppState,
    user::{
        model::{User, UserRole, UserStatus},
        repository::{UserError, UserRepository},
        service::{LoginError, UserService, normalize_identifier},
    },
};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    /// 登录标识：手机号或邮箱
    #[schema(example = "student@example.com")]
    pub identifier: String,
    #[schema(example = "P@ssw0rd!")]
    pub password: String,
}

#[derive(Serialize, ToSchema)]
pub struct LoginResponse {
    #[schema(example = "0198f2a1-3b4c-7d5e-8f90-1a2b3c4d5e6f")]
    id: Uuid,
    #[schema(example = "student@example.com")]
    email: Option<String>,
    #[schema(example = "13800138000")]
    phone: Option<String>,
    #[schema(example = "同学1234")]
    display_name: String,
    roles: Vec<UserRole>,
    token: Token,
    last_active_role: UserRole,
    #[schema(example = "https://cdn.example.com/avatar/default.png")]
    avatar_url: Option<String>,
}

/// POST /api/v1/auth/login
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "auth",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "登录成功，返回用户信息与令牌", body = LoginResponse),
        (status = 401, description = "凭证无效（用户不存在或密码错误，不可区分）"),
        (status = 403, description = "账号被禁用"),
    )
)]
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<impl IntoResponse, AppError> {
    // 1) 验凭证：业务全在 user 域，handler 只调。失败→统一错误（不可区分）
    let user_sve = UserService::new(UserRepository::new(state.pool.clone()));
    let user = user_sve
        .authenticate(&req.identifier, &req.password)
        .await
        .map_err(map_login_error)?;

    // 2) 查角色 + 发 token + 拼响应（与 login_otp 共用 build_login_response）
    let resp = build_login_response(&state, user).await?;
    Ok((StatusCode::OK, Json(resp)))
}

#[derive(Deserialize, ToSchema)]
pub struct LoginOtpRequest {
    /// 登录标识：手机号或邮箱
    #[schema(example = "13800138000")]
    pub identifier: String,
    /// 收到的 6 位验证码
    #[schema(example = "123456")]
    pub code: String,
}

/// POST /api/v1/auth/login-otp
#[utoipa::path(
    post,
    path = "/api/v1/auth/login-otp",
    tag = "auth",
    request_body = LoginOtpRequest,
    responses(
        (status = 200, description = "验证码登录成功，返回用户信息与令牌", body = LoginResponse),
        (status = 401, description = "验证码无效或已过期"),
        (status = 429, description = "请求过于频繁"),
    )
)]
pub async fn login_otp(
    State(state): State<AppState>,
    Json(req): Json<LoginOtpRequest>,
) -> Result<impl IntoResponse, AppError> {
    // 1) 先验码——purpose 写死 Login，不接受客户端传入：
    //    否则持一枚 password_reset 码的人可显式指定 purpose，拿重置码换登录会话，
    //    绕过 purpose 隔离。请求体里多带的 purpose 字段会被 serde 直接忽略。
    let id = normalize_identifier(&req.identifier);
    state
        .otp_service
        .verify(&id, Purpose::Login, &req.code)
        .await
        .map_err(map_login_otp_error)?;

    // 2) 查活跃用户
    let user_sve = UserService::new(UserRepository::new(state.pool.clone()));
    let user = user_sve
        .find_active_by_identifier(&id)
        .await
        .map_err(map_login_error)?;

    // 3) 查角色 + 发 token + 拼响应（与 login 共用）
    let resp = build_login_response(&state, user).await?;
    Ok((StatusCode::OK, Json(resp)))
}

/// 组装登录响应：查角色 + 发 token + 拼 `LoginResponse`。
/// `login` / `login_otp` 各自完成鉴权（密码 / OTP）后共用它，避免响应形状两处漂移。
/// 顺序：先查角色（只读）再发 token（refresh 落库，有副作用）——查角色失败时不留孤儿 refresh。
async fn build_login_response(state: &AppState, user: User) -> Result<LoginResponse, AppError> {
    let roles = UserRepository::new(state.pool.clone())
        .get_roles_by_user_id(&user.id)
        .await
        .map_err(map_user_error)?;
    let token = generate_token(state, &user)
        .await
        .map_err(map_session_error)?;
    Ok(LoginResponse {
        id: user.id,
        email: user.email,
        phone: user.phone,
        display_name: user.display_name,
        roles,
        last_active_role: user.last_active_role.unwrap_or(UserRole::Student),
        avatar_url: Some(user.avatar_url),
        token,
    })
}

#[derive(Serialize, ToSchema)]
pub struct Token {
    #[schema(example = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIwMTk4Zi4uLiJ9.sig")]
    access_token: String,
    #[schema(example = "kU3n7pQ2xR9vTfLmA1sB4dW6yZ0cE8gHjKlNoP-qRsT")]
    refresh_token: String,
    #[schema(example = "Bearer")]
    token_type: &'static str,
    /// access token 有效期（秒）
    #[schema(example = 900)]
    expires_in: i64,
}

async fn generate_token(state: &AppState, user: &User) -> Result<Token, SessionError> {
    // 1) 获取用户角色
    let role = user.last_active_role.unwrap_or(UserRole::Student).as_str();
    // 2) 生成 access token
    let access_token = state
        .token_manager
        .generate(user.id, role)
        .map_err(SessionError::Signing)?;

    // 3) 生成 refresh token
    let session_svc = SessionService::new(
        RefreshTokenRepository::new(state.pool.clone()),
        state.refresh_ttl,
    );
    let refresh = session_svc.issue(user.id).await?;

    Ok(Token {
        access_token,
        refresh_token: refresh.plaintext,
        token_type: TOKEN_SCHEMA,
        expires_in: state.token_manager.ttl_seconds(),
    })
}

#[derive(Deserialize, ToSchema)]
pub struct RefreshTokenRequest {
    /// 登录/轮换时下发的不透明 refresh token（base64url 随机串，非 JWT）
    #[schema(example = "kU3n7pQ2xR9vTfLmA1sB4dW6yZ0cE8gHjKlNoP-qRsT")]
    pub refresh_token: String,
}

/// POST /api/v1/auth/refresh
#[utoipa::path(
    post,
    path = "/api/v1/auth/refresh",
    tag = "auth",
    request_body = RefreshTokenRequest,
    responses(
        (status = 200, description = "轮换成功，返回新令牌", body = Token),
        (status = 401, description = "refresh token 无效、已过期或用户被禁用"),
    )
)]
pub async fn refresh_token(
    State(state): State<AppState>,
    Json(req): Json<RefreshTokenRequest>,
) -> Result<impl IntoResponse, AppError> {
    // 1)
    let session_svc = SessionService::new(
        RefreshTokenRepository::new(state.pool.clone()),
        state.refresh_ttl,
    );
    let user_id = session_svc
        .rotate(&req.refresh_token)
        .await
        .map_err(map_session_error)?;

    let user_repo = UserRepository::new(state.pool.clone());

    let user = user_repo.get_by_id(&user_id).await.map_err(|e| match e {
        UserError::NotFound => AppError::Unauthenticated("invalid refresh token".into()),
        _ => AppError::internal(e),
    })?;

    if user.status != UserStatus::Active {
        return Err(AppError::Unauthenticated("invalid refresh token".into()));
    }

    let token = generate_token(&state, &user)
        .await
        .map_err(map_session_error)?;

    Ok((StatusCode::OK, Json(token)))
}

#[derive(Deserialize, ToSchema)]
pub struct LogoutRequest {
    /// 要作废的 refresh token（不透明串，同登录下发的那枚）
    #[schema(example = "kU3n7pQ2xR9vTfLmA1sB4dW6yZ0cE8gHjKlNoP-qRsT")]
    pub refresh_token: String,
}
/// POST /api/v1/auth/logout
#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    tag = "auth",
    request_body = LogoutRequest,
    responses(
        (status = 204, description = "登出成功，refresh token 已失效"),
        (status = 401, description = "refresh token 无效"),
    )
)]
pub async fn logout(
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> Result<impl IntoResponse, AppError> {
    let session_svc = SessionService::new(
        RefreshTokenRepository::new(state.pool.clone()),
        state.refresh_ttl,
    );

    session_svc
        .logout(&req.refresh_token)
        .await
        .map_err(map_session_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct Profile {
    #[schema(example = "0198f2a1-3b4c-7d5e-8f90-1a2b3c4d5e6f")]
    pub id: Uuid,
    #[schema(example = "同学1234")]
    pub name: String,
    #[schema(example = "student@example.com")]
    pub email: Option<String>,
    #[schema(example = "13800138000")]
    pub phone: Option<String>,
    #[schema(example = "student")]
    pub role: String,
}

/// GET /api/v1/auth/me
#[utoipa::path(
    get,
    path = "/api/v1/auth/me",
    tag = "auth",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "当前登录用户信息", body = Profile),
        (status = 401, description = "未认证 / token 无效或过期"),
    )
)]
pub async fn me(
    user: AuthUser,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let user = UserRepository::new(state.pool)
        .get_by_id(&user.subject)
        .await
        .map_err(map_user_error)?;

    if user.status != UserStatus::Active {
        return Err(AppError::Unauthenticated("user is not active".into()));
    }

    Ok((
        StatusCode::OK,
        Json(Profile {
            id: user.id,
            name: user.display_name,
            email: user.email,
            phone: user.phone,
            role: user
                .last_active_role
                .unwrap_or(UserRole::Student)
                .as_str()
                .to_string(),
        }),
    ))
}

fn map_login_otp_error(err: OtpServiceError) -> AppError {
    match err {
        OtpServiceError::InvalidCode => AppError::Unauthenticated("invalid code".into()),
        OtpServiceError::RateLimited => AppError::TooManyRequests,
        _ => AppError::internal(err),
    }
}

fn map_user_error(err: UserError) -> AppError {
    match err {
        UserError::NotFound => AppError::Unauthenticated("user not found".into()),
        _ => AppError::internal(err),
    }
}

fn map_session_error(err: SessionError) -> AppError {
    match err {
        SessionError::InvalidRefreshToken => {
            AppError::Unauthenticated("invalid refresh token".into())
        }
        SessionError::Repository(e) => AppError::internal(e),
        SessionError::Signing(e) => AppError::internal(e),
    }
}

fn map_login_error(err: LoginError) -> AppError {
    match err {
        // 用户不存在 / 密码错 —— 统一 401，不可区分（安全铁律）
        LoginError::InvalidCredentials => AppError::Unauthenticated("invalid credentials".into()),
        // 账号被禁：密码已验证后才可能到这，可如实告知
        LoginError::AccountDisabled => AppError::Forbidden,
        // 仓储错 → 500，隐藏 cause
        LoginError::Repository(e) => AppError::internal(e),
    }
}
