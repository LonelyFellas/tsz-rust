use axum::extract::FromRequestParts;
use uuid::Uuid;

use crate::constant::TOKEN_SCHEMA;
use crate::{
    error::{AppError, ErrorCode},
    state::AppState,
};

#[derive(Debug)]
pub struct AuthUser {
    pub subject: Uuid,
    pub role: String,
    pub security_version: i64,
}

/// 仅用于读取本人资料、绑定和账号安全流程；业务 handler 应使用 AuthUser。
#[derive(Debug)]
pub struct SessionUser(pub AuthUser);

impl FromRequestParts<AppState> for SessionUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        authenticate(parts, state, false).await.map(Self)
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        authenticate(parts, state, true).await
    }
}

async fn authenticate(
    parts: &mut axum::http::request::Parts,
    state: &AppState,
    require_phone: bool,
) -> Result<AuthUser, AppError> {
    // 1) 取Authorization 头 -> 缺 -> Error(Unauthenticated)
    let header_value = parts
        .headers
        .get("Authorization")
        .ok_or_else(invalid_token)?;
    // 2) 拆“Bearer ” -> 缺 -> Error(Unauthenticated)
    let (scheme, token) = header_value
        .to_str()
        .map_err(|_| invalid_token())?
        .split_once(' ')
        .ok_or_else(invalid_token)?;

    if !scheme.eq_ignore_ascii_case(TOKEN_SCHEMA) {
        return Err(invalid_token());
    }

    // 3) state.token_manager.parse(token) → Expired/Invalid 都 → Err(Unauthenticated)
    //    （决策②:两种都笼统 401,不区分）
    let user = state
        .token_manager
        .parse(token)
        .map_err(|_| invalid_token())?;
    let account = sqlx::query_as::<_, (i64, Option<String>)>(
        "SELECT security_version, phone FROM users WHERE id = $1 AND status = 'active'",
    )
    .bind(user.subject)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::internal)?;
    if account.as_ref().map(|row| row.0) != Some(user.security_version) {
        return Err(invalid_token());
    }
    if require_phone && account.is_some_and(|(_, phone)| phone.is_none()) {
        return Err(AppError::forbidden(
            ErrorCode::PhoneBindingRequired,
            "phone binding required",
        ));
    }
    Ok(AuthUser {
        subject: user.subject,
        role: user.role,
        security_version: user.security_version,
    })
}

fn invalid_token() -> AppError {
    AppError::unauthorized(ErrorCode::InvalidToken, "invalid token")
}
