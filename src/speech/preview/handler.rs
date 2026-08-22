use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

use crate::{
    admin::{AdminAuth, authorization::require_active_admin},
    api::ApiJson,
    error::{AppError, ErrorCode},
    platform::storage::StorageSpace,
    speech::{
        SpeechErrorKind,
        preview::{
            PreviewRepository, PreviewService, PreviewServiceError,
            dto::{CreatePreviewRequest, PreviewResponse, VoiceListResponse},
        },
    },
    state::AppState,
};

fn service(state: &AppState) -> PreviewService {
    let speech_space = StorageSpace::parse("speech").expect("constant space is valid");
    PreviewService::new(
        PreviewRepository::new(state.pool.clone()),
        state.redis.clone(),
        state.speech_provider.clone(),
        state.object_storage.get(&speech_space).ok(),
    )
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/speech/voices",
    tag = "admin-speech",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "可用发音人目录", body = VoiceListResponse),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 500, description = "目录查询失败")
    )
)]
pub async fn list_voices(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state).list_voices().await.map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/speech/previews",
    tag = "admin-speech",
    security(("bearer_auth" = [])),
    request_body = CreatePreviewRequest,
    responses(
        (status = 200, description = "试听缓存命中或生成成功", body = PreviewResponse),
        (status = 400, description = "RichText 或语音参数非法"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "voice alias 不存在或未启用"),
        (status = 409, description = "相同试听正在生成"),
        (status = 422, description = "请求结构非法或包含未知字段"),
        (status = 429, description = "语音供应商限流"),
        (status = 503, description = "语音供应商、Redis 或 speech storage 不可用"),
        (status = 500, description = "缓存数据库失败")
    )
)]
pub async fn create_preview(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(request): ApiJson<CreatePreviewRequest>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .create_preview(request)
        .await
        .map_err(map_error)?;
    tracing::info!(cache_status = ?response.cache_status, "speech preview served");
    Ok((StatusCode::OK, Json(response)))
}

fn map_error(error: PreviewServiceError) -> AppError {
    match error {
        PreviewServiceError::VoiceNotFound => {
            AppError::not_found_with_code(ErrorCode::SpeechVoiceNotFound, "speech voice not found")
        }
        PreviewServiceError::InvalidRequest(_) => {
            AppError::bad_request(ErrorCode::InvalidSpeechPreview, "invalid speech preview")
        }
        PreviewServiceError::InProgress => AppError::conflict(
            ErrorCode::SpeechPreviewInProgress,
            None,
            "speech preview is being generated",
        ),
        PreviewServiceError::Provider(error) if error.kind == SpeechErrorKind::InvalidRequest => {
            AppError::bad_request(
                ErrorCode::InvalidSpeechPreview,
                "speech provider rejected request",
            )
        }
        PreviewServiceError::Provider(error) if error.kind == SpeechErrorKind::RateLimited => {
            AppError::rate_limited(ErrorCode::SpeechRateLimited, "speech provider rate limited")
        }
        PreviewServiceError::Provider(_)
        | PreviewServiceError::ProviderNotConfigured
        | PreviewServiceError::ProviderMismatch => AppError::unavailable(
            ErrorCode::SpeechProviderUnavailable,
            "speech provider unavailable",
        ),
        PreviewServiceError::Storage(_) => AppError::unavailable(
            ErrorCode::SpeechStorageUnavailable,
            "speech storage unavailable",
        ),
        PreviewServiceError::Lock => AppError::unavailable(
            ErrorCode::ServiceUnavailable,
            "speech preview coordination unavailable",
        ),
        PreviewServiceError::Database(error) => AppError::internal(error),
    }
}
