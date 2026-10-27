use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};

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
        state.pool.clone(),
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
    params(("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 返回已验证模型及能力；旧客户端仅普通目录"), ("X-TSZ-Voice-Catalog"=Option<String>,Header,description="v1 返回全语种浏览目录、当前三态归属及真实能力证据；合成仅开放已验证英语")),
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
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    let mut response = service(&state)
        .list_voices(auth.subject)
        .await
        .map_err(map_error)?;
    if !crate::lexicon::voice_versions::models_supported(&headers) {
        response.items.retain(|voice| {
            !voice.alias.starts_with("azure-hd-flash-")
                && voice
                    .metadata
                    .as_ref()
                    .is_none_or(|metadata| metadata.model != "hd_flash")
        });
    }
    if headers
        .get("x-tsz-voice-catalog")
        .is_none_or(|value| value != "v1")
    {
        response
            .items
            .retain(|voice| crate::speech::locale::EnglishLocale::parse(&voice.locale).is_some());
        for voice in &mut response.items {
            voice.accent = None;
            voice.accent_revision = None;
            voice.synthesis_available = None;
            voice.capability_evidence = None;
        }
    }
    Ok((
        StatusCode::OK,
        [
            ("x-tsz-voice-models", "v1"),
            ("x-tsz-voice-catalog", "v1"),
            ("vary", "x-tsz-voice-models, x-tsz-voice-catalog"),
        ],
        Json(response),
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/speech/previews",
    tag = "admin-speech",
    security(("bearer_auth" = [])),
    request_body = CreatePreviewRequest,
    params(("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型"),("X-TSZ-Test-Run" = Option<String>, Header, description = "仅允许服务端白名单账号使用的8至80位独占测试批次；资源写入temporary-tests前缀")),
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
    headers: HeaderMap,
    ApiJson(request): ApiJson<CreatePreviewRequest>,
) -> Result<impl IntoResponse, AppError> {
    require_active_admin(&state, &auth).await?;
    if !crate::lexicon::voice_versions::models_supported(&headers) {
        let voice = PreviewRepository::new(state.pool.clone())
            .voice_by_alias(&request.voice_alias)
            .await
            .map_err(AppError::internal)?;
        if voice.is_some_and(|record| record.voice.is_hd_flash()) {
            return Err(AppError::bad_request(
                ErrorCode::InvalidSpeechPreview,
                "该模型需要新版配音能力，请刷新页面",
            ));
        }
    }
    let (state, run) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    let response = service(&state)
        .with_test_run(run)
        .create_preview(auth.subject, request)
        .await
        .map_err(map_error)?;
    tracing::info!(cache_status = ?response.cache_status, "speech preview served");
    Ok((StatusCode::OK, Json(response)))
}

#[utoipa::path(post,path="/api/v1/admin/speech/ssml",tag="admin-speech",security(("bearer_auth"=[])),request_body=super::dto::RenderSsmlRequest,responses((status=200,body=super::dto::RenderSsmlResponse),(status=400),(status=403)))]
pub async fn render_ssml(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<super::dto::RenderSsmlRequest>,
) -> Result<Json<super::dto::RenderSsmlResponse>, AppError> {
    let permission = crate::admin::permissions::load(&state, &auth).await?;
    permission.require("speech.generate")?;
    permission.require("words.access")?;
    let mut conn = state.pool.acquire().await.map_err(AppError::internal)?;
    let (request, _) = crate::speech::generated::prepare_request(
        &mut conn,
        &input.source,
        &input.setting,
        None,
        None,
    )
    .await?;
    Ok(Json(super::dto::RenderSsmlResponse {
        ssml: crate::speech::build_ssml(&request).map_err(|_| {
            AppError::bad_request(ErrorCode::InvalidSpeechPreview, "invalid synthesis request")
        })?,
    }))
}

#[utoipa::path(post,path="/api/v1/admin/speech/previews/ssml",tag="admin-speech",security(("bearer_auth"=[])),request_body=super::dto::ManualPreviewRequest,responses((status=200,body=PreviewResponse),(status=400),(status=403),(status=503)))]
pub async fn manual_preview(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    ApiJson(input): ApiJson<super::dto::ManualPreviewRequest>,
) -> Result<Json<PreviewResponse>, AppError> {
    require_active_admin(&state, &auth).await?;
    let (state, run) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    Ok(Json(
        service(&state)
            .with_test_run(run)
            .create_manual_preview(auth.subject, input)
            .await
            .map_err(map_error)?,
    ))
}

#[utoipa::path(get,path="/api/v1/admin/speech/previews/{hash}/content",tag="admin-speech",security(("bearer_auth"=[])),params(("hash"=String,Path)),responses((status=200,content_type="application/octet-stream",body=Vec<u8>),(status=400),(status=403),(status=404),(status=503)))]
pub async fn preview_content(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    axum::extract::Path(hash): axum::extract::Path<String>,
) -> Result<axum::response::Response, AppError> {
    require_active_admin(&state, &auth).await?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::bad_request(
            ErrorCode::InvalidPathParameter,
            "invalid preview hash",
        ));
    }
    let mut bytes = [0u8; 32];
    for (index, value) in bytes.iter_mut().enumerate() {
        *value =
            u8::from_str_radix(&hash[index * 2..index * 2 + 2], 16).map_err(AppError::internal)?;
    }
    let (state, run) = crate::test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    let (mime, content) = service(&state)
        .with_test_run(run)
        .read_content(auth.subject, &bytes)
        .await
        .map_err(map_error)?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (axum::http::header::CACHE_CONTROL, "no-store".into()),
        ],
        content,
    )
        .into_response())
}

fn map_error(error: PreviewServiceError) -> AppError {
    match error {
        PreviewServiceError::Authorization(error) => error,
        PreviewServiceError::VoiceNotFound => {
            AppError::not_found_with_code(ErrorCode::SpeechVoiceNotFound, "speech voice not found")
        }
        PreviewServiceError::InvalidRequest(
            crate::speech::SpeechModelError::UnsupportedPhonemeLocale,
        ) => AppError::bad_request(
            ErrorCode::InvalidSpeechPreview,
            "指定音素尚未验证适用于该原生地区，请明确选择自然朗读或使用匹配地区的音色",
        ),
        PreviewServiceError::InvalidRequest(
            crate::speech::SpeechModelError::UnsupportedModelCapability,
        ) => AppError::bad_request(
            ErrorCode::InvalidSpeechPreview,
            crate::speech::SpeechModelError::UnsupportedModelCapability.to_string(),
        ),
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
        // 生成任务 panic 才会走到这里，属于 bug 而非可预期故障，按 500 暴露。
        PreviewServiceError::Task(error) => AppError::internal(error),
    }
}
