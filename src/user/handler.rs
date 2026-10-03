use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    api::ApiJson,
    auth::{
        extract::AuthUser,
        handler::{UserProfile, load_user_profile},
    },
    error::{AppError, ErrorCode},
    state::AppState,
    user::{model::DisplayName, repository::UserRepository, service::UserService},
};

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateProfileRequest {
    #[schema(example = "新昵称")]
    pub display_name: String,
}

#[derive(Serialize, ToSchema)]
pub struct UpdateProfileResponse {
    pub user: UserProfile,
}

#[utoipa::path(
    patch,
    path = "/api/v1/me",
    tag = "user",
    security(("bearer_auth" = [])),
    request_body = UpdateProfileRequest,
    responses(
        (status = 200, description = "本人昵称已保存", body = UpdateProfileResponse),
        (status = 400, description = "昵称无效或 JSON 语法错误"),
        (status = 401, description = "Web 会话无效"),
        (status = 413, description = "请求体超过 2 KiB"),
        (status = 422, description = "请求体字段无效"),
        (status = 500, description = "服务内部错误；同昵称可安全重试"),
    )
)]
pub async fn update_profile(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(request): ApiJson<UpdateProfileRequest>,
) -> Result<Json<UpdateProfileResponse>, AppError> {
    let name = DisplayName::parse(&request.display_name).map_err(|error| {
        AppError::validation(
            ErrorCode::InvalidDisplayName,
            "display_name",
            error.to_string(),
        )
    })?;
    let user = UserService::new(UserRepository::new(state.pool.clone()))
        .update_display_name(auth.subject, auth.security_version, name)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::unauthorized(ErrorCode::InvalidToken, "invalid token"))?;
    let user = load_user_profile(&state, &user).await?;
    Ok(Json(UpdateProfileResponse { user }))
}
