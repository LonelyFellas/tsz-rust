use super::{dto::*, service};
use crate::{
    api::{ApiPath, ApiQuery},
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
    lexicon::audio_assets::dto::AudioAssetUrlResponse,
    platform::storage::{ObjectKey, StorageSpace},
    state::AppState,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, header},
    routing::get,
};
use chrono::{DateTime, Utc};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/dictionary", get(search))
        .route("/api/v1/dictionary/entries/{id}", get(detail))
        .route(
            "/api/v1/dictionary/entries/{id}/audio/{asset_id}",
            get(audio),
        )
}
fn reading_response<T: serde::Serialize>(
    headers: &HeaderMap,
    value: T,
) -> Result<(HeaderMap, Json<serde_json::Value>), AppError> {
    let mut value = serde_json::to_value(value).map_err(AppError::internal)?;
    if !crate::lexicon::voice_versions::dubbing_supported(headers) {
        crate::lexicon::voice_versions::project_dubbing(&mut value);
    }
    if !crate::lexicon::voice_versions::models_supported(headers) {
        fn project_source(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(fields) => {
                    fields.remove("source_dialect");
                    fields.values_mut().for_each(project_source);
                }
                serde_json::Value::Array(items) => items.iter_mut().for_each(project_source),
                _ => {}
            }
        }
        project_source(&mut value);
    }
    if !crate::lexicon::voice_versions::extended_supported(headers) {
        crate::lexicon::voice_versions::project_extended(&mut value);
    }
    let mut response_headers = private_headers();
    response_headers.insert(
        header::VARY,
        "x-tsz-voice-versions, x-tsz-voice-locales, x-tsz-voice-models"
            .parse()
            .unwrap(),
    );
    response_headers.insert(
        crate::lexicon::voice_versions::LOCALES_HEADER,
        "v1".parse().unwrap(),
    );
    response_headers.insert(
        crate::lexicon::voice_versions::MODELS_HEADER,
        "v1".parse().unwrap(),
    );
    Ok((response_headers, Json(value)))
}

fn private_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    headers
}
#[utoipa::path(get,path="/api/v1/dictionary",tag="dictionary",params(DictionaryQuery,("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型；旧读取隐藏模型配置并保护旧写"),("X-TSZ-Voice-Versions"=Option<String>,Header,description="v2 保留正式音频"),("X-TSZ-Voice-Locales"=Option<String>,Header,description="v1 返回原生英语地区及每项 accent_group；缺省仅返回英美旧格式")),security(("bearer_auth"=[])),
responses((status=200,description="当前发布词条，按英文拼写包含匹配；q 不能为空",body=DictionaryPage),(status=400,description="查询无效"),(status=401,description="需要登录"),(status=403,description="需要绑定手机号")))]
pub async fn search(
    State(state): State<AppState>,
    _auth: AuthUser,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<DictionaryQuery>,
) -> Result<(HeaderMap, Json<serde_json::Value>), AppError> {
    let mut page = service::search(&state.pool, query).await?;
    if !crate::lexicon::voice_versions::models_supported(&headers) {
        for entry in &mut page.items {
            entry.audio.retain(|audio| !audio.hd_flash);
        }
    }
    reading_response(&headers, page)
}
#[utoipa::path(get,path="/api/v1/dictionary/entries/{id}",tag="dictionary",params(("id"=Uuid,Path),("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 支持已验证模型；旧读取隐藏模型配置并保护旧写"),("X-TSZ-Voice-Versions"=Option<String>,Header,description="v2 保留正式音频"),("X-TSZ-Voice-Locales"=Option<String>,Header,description="v1 返回原生英语地区及每项 accent_group；缺省仅返回英美旧格式")),security(("bearer_auth"=[])),
responses((status=200,body=DictionaryEntry),(status=401,description="需要登录"),(status=403,description="需要绑定手机号"),(status=404,description="词条未发布、已撤回或不存在")))]
pub async fn detail(
    State(state): State<AppState>,
    _auth: AuthUser,
    headers: HeaderMap,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<(HeaderMap, Json<serde_json::Value>), AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let mut item = service::entries(&mut tx, &[id])
        .await?
        .pop()
        .ok_or_else(|| AppError::not_found("词条暂不可用"))?;
    tx.commit().await.map_err(AppError::internal)?;
    if !crate::lexicon::voice_versions::models_supported(&headers) {
        item.audio.retain(|audio| !audio.hd_flash);
    }
    reading_response(&headers, item)
}
#[utoipa::path(get,path="/api/v1/dictionary/entries/{id}/audio/{asset_id}",tag="dictionary",params(("id"=Uuid,Path),("asset_id"=Uuid,Path),("X-TSZ-Voice-Models"=Option<String>,Header,description="v1 读取已发布模型音频")),security(("bearer_auth"=[])),
responses((status=200,body=AudioAssetUrlResponse),(status=401,description="需要登录"),(status=403,description="需要绑定手机号"),(status=404,description="当前发布词条未引用该音频"),(status=501,description="音频存储未配置"),(status=503,description="音频服务不可用")))]
pub async fn audio(
    State(state): State<AppState>,
    _auth: AuthUser,
    headers: HeaderMap,
    ApiPath((id, asset_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<(HeaderMap, Json<AudioAssetUrlResponse>), AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    if !service::audio_available(
        &mut tx,
        id,
        asset_id,
        crate::lexicon::voice_versions::models_supported(&headers),
    )
    .await?
    {
        return Err(AppError::not_found("音频暂不可用"));
    }
    let object_key: String = sqlx::query_scalar("SELECT object_key FROM lexicon.audio_assets WHERE id=$1 AND reclamation_started_at IS NULL").bind(asset_id).fetch_optional(&mut *tx).await.map_err(AppError::internal)?.ok_or_else(|| AppError::not_found("音频暂不可用"))?;
    let storage = state
        .object_storage
        .get(&StorageSpace::parse("audio").expect("constant space"))
        .map_err(|_| {
            AppError::request_error(ErrorCode::AudioStorageNotConfigured, "音频存储未配置")
        })?;
    let key = ObjectKey::parse(object_key).map_err(AppError::internal)?;
    let signed = storage.presign_read(&key).await.map_err(|_| {
        AppError::request_error(ErrorCode::SpeechStorageUnavailable, "音频服务暂不可用")
    })?;
    let ttl = signed.expires_in();
    tx.commit().await.map_err(AppError::internal)?;
    Ok((
        private_headers(),
        Json(AudioAssetUrlResponse {
            url: signed.url().to_owned(),
            expires_at: DateTime::<Utc>::from(std::time::SystemTime::now() + ttl),
            url_expires_in_seconds: ttl.as_secs(),
        }),
    ))
}
