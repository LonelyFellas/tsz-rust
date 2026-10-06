use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    api::ApiJson,
    auth::{
        extract::{AuthUser, SessionUser},
        handler::{UserProfile, load_user_profile},
    },
    error::{AppError, ErrorCode},
    state::AppState,
    user::{
        model::{DisplayName, LearningSettings, UserRole, UserStatus},
        repository::{SaveLearningSettingsError, UserError, UserRepository},
        service::UserService,
    },
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
        (status = 403, description = "phone_binding_required：需先绑定手机号"),
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

#[derive(Serialize, ToSchema)]
pub struct MeResponse {
    pub user: UserProfile,
    pub active_role: UserRole,
    #[schema(required = true)]
    pub learning_settings: Option<LearningSettings>,
    pub onboarded: bool,
}

#[derive(Serialize, ToSchema)]
pub struct LearningSettingsResponse {
    pub learning_settings: LearningSettings,
    pub onboarded: bool,
}

#[utoipa::path(
    get,
    path = "/api/v1/me",
    tag = "user",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "当前用户和真实学习配置；未配置为 null", body = MeResponse),
        (status = 401, description = "Web 会话无效"),
        (status = 500, description = "服务内部错误"),
    )
)]
pub async fn me(
    State(state): State<AppState>,
    SessionUser(auth): SessionUser,
) -> Result<Json<MeResponse>, AppError> {
    let repository = UserRepository::new(state.pool.clone());
    let user = repository
        .get_by_id(&auth.subject)
        .await
        .map_err(|error| match error {
            UserError::NotFound => AppError::unauthorized(ErrorCode::InvalidToken, "invalid token"),
            other => AppError::internal(other),
        })?;
    if user.status != UserStatus::Active || user.security_version != auth.security_version {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
    let user = load_user_profile(&state, &user).await?;
    let student = user.roles.contains(&UserRole::Student);
    let learning_settings = if student {
        repository
            .learning_settings(auth.subject)
            .await
            .map_err(AppError::internal)?
    } else {
        None
    };
    Ok(Json(MeResponse {
        active_role: user.active_role,
        user,
        onboarded: !student || learning_settings.is_some(),
        learning_settings,
    }))
}

#[utoipa::path(
    put,
    path = "/api/v1/me/learning-settings",
    tag = "user",
    security(("bearer_auth" = [])),
    request_body = LearningSettings,
    responses(
        (status = 200, description = "首次配置或修改英美偏好；难度首次保存后不可改，同载荷可重试", body = LearningSettingsResponse),
        (status = 400, description = "JSON 语法错误"),
        (status = 401, description = "Web 会话无效"),
        (status = 403, description = "没有学生身份，或 phone_binding_required：需先绑定手机号"),
        (status = 409, description = "cefr_level_locked：已保存难度不可修改，两项均不变"),
        (status = 413, description = "请求体超过 2 KiB"),
        (status = 422, description = "缺字段、非法枚举或额外字段"),
        (status = 500, description = "服务内部错误"),
    )
)]
pub async fn update_learning_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(settings): ApiJson<LearningSettings>,
) -> Result<Json<LearningSettingsResponse>, AppError> {
    let learning_settings = UserRepository::new(state.pool.clone())
        .save_learning_settings(auth.subject, auth.security_version, settings)
        .await
        .map_err(|error| match error {
            SaveLearningSettingsError::InvalidSession => {
                AppError::unauthorized(ErrorCode::InvalidToken, "invalid token")
            }
            SaveLearningSettingsError::StudentRequired => AppError::forbidden(
                ErrorCode::Forbidden,
                "learning settings require a student profile",
            ),
            SaveLearningSettingsError::LevelLocked => AppError::conflict(
                ErrorCode::CefrLevelLocked,
                Some("cefr_level"),
                "CEFR level cannot be changed after initial setup",
            ),
            other => AppError::internal(other),
        })?;
    Ok(Json(LearningSettingsResponse {
        learning_settings,
        onboarded: true,
    }))
}
