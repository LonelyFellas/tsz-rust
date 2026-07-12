use crate::{
    auth::extract::AuthUser,
    constant::TOKEN_SCHEMA,
    error::AppError,
    session::{
        repository::RefreshTokenRepository,
        service::{SessionError, SessionService},
    },
    state::AppState,
    user::{
        model::{UserRole, UserStatus},
        repository::{UserError, UserRepository},
        service::{LoginError, UserService},
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
    access_token: String,
    refresh_token: String,
    pub token_type: &'static str, // "Bearer"
    pub expires_in: i64,          // access 剩余秒
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

    // 2) 签发 access token (TokenManager 从State 拿， 不现建)
    let role = user.last_active_role.unwrap_or(UserRole::Student).as_str();
    let access_token = state
        .token_manager
        .generate(user.id, role)
        .map_err(AppError::internal)?;

    // 3) 签发 refresh token
    let session_svc = SessionService::new(
        RefreshTokenRepository::new(state.pool.clone()),
        state.refresh_ttl,
    );

    let refresh = session_svc
        .issue(user.id)
        .await
        .map_err(AppError::internal)?;

    // 4) 200 + OAuth 形状
    Ok((
        StatusCode::OK,
        Json(LoginResponse {
            access_token,
            refresh_token: refresh.plaintext,
            token_type: TOKEN_SCHEMA,
            expires_in: state.token_manager.ttl_seconds(),
        }),
    ))
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

    let access_token = state
        .token_manager
        .generate(
            user.id,
            user.last_active_role.unwrap_or(UserRole::Student).as_str(),
        )
        .map_err(AppError::internal)?;
    let refresh_token = session_svc
        .issue(user.id)
        .await
        .map_err(AppError::internal)?;

    Ok((
        StatusCode::OK,
        Json(LoginResponse {
            access_token,
            refresh_token: refresh_token.plaintext,
            token_type: TOKEN_SCHEMA,
            expires_in: state.token_manager.ttl_seconds(),
        }),
    ))
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
