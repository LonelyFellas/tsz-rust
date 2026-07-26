use axum::extract::FromRequestParts;
use uuid::Uuid;

use crate::{admin::AdminRole, constant::TOKEN_SCHEMA, error::AppError, state::AppState};

pub struct AdminAuth {
    pub subject: Uuid,
    /// 目前无人消费；二期 `RequireSuperAdmin` 门禁从这里读，勿删。
    pub role: AdminRole,
}

impl FromRequestParts<AppState> for AdminAuth {
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

        // 3) admin_token_manager.parse → Expired/Invalid 都笼统 401（与 web AuthUser 同决策）；
        //    realm 隔离由 aud 校验兑现——web token 在这里天然被拒。
        let claims = state
            .admin_token_manager
            .parse(token)
            .map_err(|_| AppError::Unauthenticated("invalid token".into()))?;

        // 4) role claim 认不出 = token 伪造或版本漂移，同样 fail-closed 成 401。
        let role = AdminRole::parse(claims.role.as_str())
            .ok_or(AppError::Unauthenticated("invalid token".into()))?;

        Ok(AdminAuth {
            subject: claims.subject,
            role,
        })
    }
}
