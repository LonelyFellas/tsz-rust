use axum::extract::FromRequestParts;
use uuid::Uuid;

use crate::constant::TOKEN_SCHEMA;
use crate::{error::AppError, state::AppState};

#[derive(Debug)]
pub struct AuthUser {
    pub subject: Uuid,
    pub role: String,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // 1) 取Authorization 头 -> 缺 -> Error(Unauthenticated)
        let header_value = parts
            .headers
            .get("Authorization")
            .ok_or(AppError::Unauthenticated(
                "missing authorization header".into(),
            ))?;
        // 2) 拆“Bearer ” -> 缺 -> Error(Unauthenticated)
        let (scheme, token) = header_value
            .to_str()
            .map_err(|_| AppError::Unauthenticated("invalid authorization header".into()))?
            .split_once(' ')
            .ok_or(AppError::Unauthenticated("missing token schema".into()))?;

        if !scheme.eq_ignore_ascii_case(TOKEN_SCHEMA) {
            return Err(AppError::Unauthenticated("invalid token schema".into()));
        }

        // 3) state.token_manager.parse(token) → Expired/Invalid 都 → Err(Unauthenticated)
        //    （决策②:两种都笼统 401,不区分）
        let user = state
            .token_manager
            .parse(token)
            .map_err(|_| AppError::Unauthenticated("invalid token".into()))?;
        // 4) Ok(AuthUser { subject: claims.subject, role: claims.role })
        Ok(AuthUser {
            subject: user.subject,
            role: user.role,
        })
    }
}
