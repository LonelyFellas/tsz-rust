use crate::{
    api::ApiJson,
    auth::extract::SessionUser,
    error::{AppError, ErrorCode},
    i18n::{InterfaceLocaleV1, InterfaceSurface},
    state::AppState,
};
use axum::{Json, extract::State, http::header};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct UserInterfacePreferences {
    #[schema(required = true)]
    pub interface_locale: Option<InterfaceLocaleV1>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateUserInterfacePreferences {
    #[serde(deserialize_with = "Option::deserialize")]
    #[schema(required = true)]
    pub interface_locale: Option<InterfaceLocaleV1>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UserInterfacePreferencesResponse {
    pub preferences: UserInterfacePreferences,
}

fn invalid_session() -> AppError {
    AppError::unauthorized(ErrorCode::InvalidToken, "invalid token")
}

#[utoipa::path(get, path = "/api/v1/me/preferences", tag = "user", security(("bearer_auth" = [])),
    responses((status = 200, body = UserInterfacePreferencesResponse), (status = 401, description = "invalid_token")))]
pub async fn get(
    State(state): State<AppState>,
    SessionUser(auth): SessionUser,
) -> Result<impl axum::response::IntoResponse, AppError> {
    let locale = sqlx::query_scalar::<_, Option<InterfaceLocaleV1>>("SELECT interface_locale FROM users WHERE id=$1 AND status='active' AND security_version=$2")
        .bind(auth.subject).bind(auth.security_version).fetch_optional(&state.pool).await.map_err(AppError::internal)?.ok_or_else(invalid_session)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(UserInterfacePreferencesResponse {
            preferences: UserInterfacePreferences {
                interface_locale: locale,
            },
        }),
    ))
}

#[utoipa::path(patch, path = "/api/v1/me/preferences", tag = "user", security(("bearer_auth" = [])),
    request_body = UpdateUserInterfacePreferences,
    responses((status = 200, body = UserInterfacePreferencesResponse), (status = 400, description = "invalid_json"),
        (status = 401, description = "invalid_token"), (status = 413, description = "payload_too_large"), (status = 422, description = "invalid_request_body or unsupported_language")))]
pub async fn update(
    State(state): State<AppState>,
    SessionUser(auth): SessionUser,
    ApiJson(request): ApiJson<UpdateUserInterfacePreferences>,
) -> Result<impl axum::response::IntoResponse, AppError> {
    state
        .interface_languages
        .require_supported(InterfaceSurface::Web, request.interface_locale)?;
    let locale = sqlx::query_scalar::<_, Option<InterfaceLocaleV1>>("UPDATE users SET interface_locale=$3,updated_at=NOW() WHERE id=$1 AND status='active' AND security_version=$2 RETURNING interface_locale")
        .bind(auth.subject).bind(auth.security_version).bind(request.interface_locale).fetch_optional(&state.pool).await.map_err(AppError::internal)?.ok_or_else(invalid_session)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(UserInterfacePreferencesResponse {
            preferences: UserInterfacePreferences {
                interface_locale: locale,
            },
        }),
    ))
}
