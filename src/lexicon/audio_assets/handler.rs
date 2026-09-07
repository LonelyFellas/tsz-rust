use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

use crate::{
    admin::{AdminAuth, authorization::require_active_admin},
    api::{ApiJson, ApiPath},
    error::{AppError, ErrorCode},
    lexicon::audio_assets::{
        dto::{
            AudioAssetPath, AudioAssetUrlResponse, ConfirmAudioAssetRequest,
            ConfirmAudioAssetResponse, CreateAudioUploadRequest, CreateAudioUploadResponse,
        },
        repository::AudioAssetRepository,
        service::{AudioAssetService, AudioAssetServiceError},
    },
    platform::storage::StorageSpace,
    state::AppState,
};

fn service(state: &AppState) -> AudioAssetService {
    let audio_space = StorageSpace::parse("audio").expect("constant space is valid");
    AudioAssetService::new(
        AudioAssetRepository::new(state.pool.clone()),
        state.object_storage.get(&audio_space).ok(),
    )
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/audio-assets/upload-url",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    request_body = CreateAudioUploadRequest,
    responses(
        (status = 200, description = "直传许可", body = CreateAudioUploadResponse),
        (status = 400, description = "音频类型不在白名单"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 413, description = "文件超过空间上限"),
        (status = 422, description = "请求结构非法或包含未知字段"),
        (status = 501, description = "音频存储未开通"),
        (status = 503, description = "音频存储不可用")
    )
)]
pub async fn create_audio_upload(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(request): ApiJson<CreateAudioUploadRequest>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .create_upload(request)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/lexicon/audio-assets",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    request_body = ConfirmAudioAssetRequest,
    responses(
        (status = 201, description = "音频资产已登记", body = ConfirmAudioAssetResponse),
        (status = 400, description = "对象键非法、上传未完成或类型不在白名单"),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 413, description = "对象超过空间上限"),
        (status = 422, description = "请求结构非法或包含未知字段"),
        (status = 501, description = "音频存储未开通"),
        (status = 503, description = "音频存储不可用")
    )
)]
pub async fn confirm_audio_asset(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(request): ApiJson<ConfirmAudioAssetRequest>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .confirm(auth.subject, request)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/lexicon/audio-assets/{id}/url",
    tag = "admin-lexicon",
    security(("bearer_auth" = [])),
    params(AudioAssetPath),
    responses(
        (status = 200, description = "短期只读 URL", body = AudioAssetUrlResponse),
        (status = 401, description = "管理员身份无效"),
        (status = 403, description = "账号已禁用或必须先改密"),
        (status = 404, description = "音频资产不存在或不可读"),
        (status = 501, description = "音频存储未开通"),
        (status = 503, description = "音频存储不可用")
    )
)]
pub async fn audio_asset_url(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(path): ApiPath<AudioAssetPath>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let response = service(&state)
        .presign_url(auth.subject, path.id)
        .await
        .map_err(map_error)?;
    Ok((StatusCode::OK, Json(response)))
}

fn map_error(error: AudioAssetServiceError) -> AppError {
    match error {
        AudioAssetServiceError::StorageNotConfigured => AppError::request_error(
            ErrorCode::AudioStorageNotConfigured,
            "audio storage is not configured",
        ),
        AudioAssetServiceError::UnsupportedContentType => AppError::request_error(
            ErrorCode::UnsupportedAudioContentType,
            "unsupported audio content type",
        ),
        AudioAssetServiceError::FileTooLarge => AppError::request_error(
            ErrorCode::AudioFileTooLarge,
            "audio file exceeds the size limit",
        ),
        AudioAssetServiceError::InvalidKey => {
            AppError::request_error(ErrorCode::InvalidAudioKey, "invalid audio upload key")
        }
        AudioAssetServiceError::UploadNotCompleted => AppError::request_error(
            ErrorCode::AudioUploadNotCompleted,
            "audio upload was not completed",
        ),
        AudioAssetServiceError::InvalidOriginalName => AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "original_name must be 1 to 120 characters",
        ),
        AudioAssetServiceError::NotFound => {
            AppError::not_found_with_code(ErrorCode::AudioAssetNotFound, "audio asset not found")
        }
        AudioAssetServiceError::Storage(_) => {
            AppError::unavailable(ErrorCode::ServiceUnavailable, "audio storage unavailable")
        }
        AudioAssetServiceError::Database(error) => AppError::internal(error),
    }
}
