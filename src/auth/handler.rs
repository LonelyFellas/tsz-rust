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
use uuid::Uuid;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub identifier: String,
    pub password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    id: Uuid,
    email: Option<String>,
    phone: Option<String>,
    display_name: String,
    roles: Vec<UserRole>,
    token: Token,
    last_active_role: UserRole,
    avatar_url: Option<String>,
}

/// POST /api/v1/auth/login
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
    let resp = build_login_response(&state, &user_sve, user).await?;
    Ok((StatusCode::OK, Json(resp)))
}

#[derive(Deserialize)]
pub struct LoginOtpRequest {
    pub identifier: String,
    pub code: String,
}

/// POST /api/v1/auth/login-otp
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
    let resp = build_login_response(&state, &user_sve, user).await?;
    Ok((StatusCode::OK, Json(resp)))
}

/// 组装登录响应：查角色 + 发 token + 拼 `LoginResponse`。
/// `login` / `login_otp` 各自完成鉴权（密码 / OTP）后共用它，避免响应形状两处漂移。
/// 顺序：先查角色（只读）再发 token（refresh 落库，有副作用）——查角色失败时不留孤儿 refresh。
async fn build_login_response(
    state: &AppState,
    user_sve: &UserService,
    user: User,
) -> Result<LoginResponse, AppError> {
    let roles = user_sve
        .query_roles_by_user_id(&user.id)
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

#[derive(Serialize)]
struct Token {
    access_token: String,
    refresh_token: String,
    token_type: &'static str,
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

#[derive(Deserialize)]
pub struct RefreshTokenRequest {
    pub refresh_token: String,
}

/// POST /api/v1/auth/refresh
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

#[derive(Deserialize)]
pub struct LogoutRequest {
    pub refresh_token: String,
}
/// POST /api/v1/auth/logout
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

#[derive(Serialize)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: String,
}

/// GET /api/v1/auth/me
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
