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
        (status = 400, description = "对象键非法、上传未完成、类型不在白名单或 original_name 非法"),
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
        // 域内字段校验走 400 + field，与词条标注等既有字段错一致；
        // 422 invalid_request_body 只表示 JSON 反序列化失败，见 docs/api-errors.md。
        AudioAssetServiceError::InvalidOriginalName => AppError::validation(
            ErrorCode::InvalidRequestBody,
            "original_name",
            "original_name must be 1 to 120 characters without control characters",
        ),
        AudioAssetServiceError::NotFound => {
            AppError::not_found_with_code(ErrorCode::AudioAssetNotFound, "audio asset not found")
        }
        // 对外只给一个笼统的 503，但服务端要留下痕迹：copy 半成功之类的故障
        // 只能靠这条日志定位，响应体里不会有任何线索。
        AudioAssetServiceError::Storage(error) => {
            tracing::error!(
                error = %error,
                error_kind = "audio_storage",
                "audio asset storage operation failed"
            );
            AppError::unavailable(ErrorCode::ServiceUnavailable, "audio storage unavailable")
        }
        AudioAssetServiceError::Database(error) => AppError::internal(error),
        // 搬运任务 panic 才会走到这里，属于 bug 而非可预期故障，按 500 暴露。
        AudioAssetServiceError::Task(error) => AppError::internal(error),
    }
}
