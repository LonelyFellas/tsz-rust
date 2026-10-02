use std::sync::Arc;

use axum::extract::FromRequestParts;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::{
    admin::AdminRole,
    constant::TOKEN_SCHEMA,
    error::{AppError, ErrorCode},
    state::AppState,
};

#[derive(Clone)]
pub struct AdminAuth {
    pub subject: Uuid,
    pub security_version: i64,
    /// Token role is not trusted for current authorization; database identity is reused below.
    pub role: AdminRole,
    pub(crate) active_admin: Arc<OnceCell<crate::admin::Admin>>,
    pub(crate) authorization: Arc<OnceCell<crate::admin::permissions::AdminAuthorization>>,
}

impl FromRequestParts<AppState> for AdminAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(auth) = parts.extensions.get::<Self>() {
            return Ok(auth.clone());
        }
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

        // 3) admin_token_manager.parse → Expired/Invalid 都笼统 401（与 web AuthUser 同决策）；
        //    realm 隔离由 aud 校验兑现——web token 在这里天然被拒。
        let claims = state
            .admin_token_manager
            .parse(token)
            .map_err(|_| invalid_token())?;

        // 4) role claim 认不出 = token 伪造或版本漂移，同样 fail-closed 成 401。
        let role = AdminRole::parse(claims.role.as_str()).ok_or_else(invalid_token)?;

        let current_version =
            sqlx::query_scalar::<_, i64>("SELECT security_version FROM admins WHERE id = $1")
                .bind(claims.subject)
                .fetch_optional(&state.pool)
                .await
                .map_err(AppError::internal)?;
        if current_version != Some(claims.security_version) {
            return Err(invalid_token());
        }

        let auth = AdminAuth {
            subject: claims.subject,
            security_version: claims.security_version,
            role,
            active_admin: Arc::new(OnceCell::new()),
            authorization: Arc::new(OnceCell::new()),
        };
        parts.extensions.insert(auth.clone());
        Ok(auth)
    }
}

fn invalid_token() -> AppError {
    AppError::unauthorized(ErrorCode::InvalidToken, "invalid token")
}
