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
fn private_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    headers
}
#[utoipa::path(get,path="/api/v1/dictionary",tag="dictionary",params(DictionaryQuery),security(("bearer_auth"=[])),
responses((status=200,description="当前发布词条，按英文拼写包含匹配；q 不能为空",body=DictionaryPage),(status=400,description="查询无效"),(status=401,description="需要登录"),(status=403,description="需要绑定手机号")))]
pub async fn search(
    State(state): State<AppState>,
    _auth: AuthUser,
    ApiQuery(query): ApiQuery<DictionaryQuery>,
) -> Result<(HeaderMap, Json<DictionaryPage>), AppError> {
    Ok((
        private_headers(),
        Json(service::search(&state.pool, query).await?),
    ))
}
#[utoipa::path(get,path="/api/v1/dictionary/entries/{id}",tag="dictionary",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),
responses((status=200,body=DictionaryEntry),(status=401,description="需要登录"),(status=403,description="需要绑定手机号"),(status=404,description="词条未发布、已撤回或不存在")))]
pub async fn detail(
    State(state): State<AppState>,
    _auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<(HeaderMap, Json<DictionaryEntry>), AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let item = service::entries(&mut tx, &[id])
        .await?
        .pop()
        .ok_or_else(|| AppError::not_found("词条暂不可用"))?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok((private_headers(), Json(item)))
}
#[utoipa::path(get,path="/api/v1/dictionary/entries/{id}/audio/{asset_id}",tag="dictionary",params(("id"=Uuid,Path),("asset_id"=Uuid,Path)),security(("bearer_auth"=[])),
responses((status=200,body=AudioAssetUrlResponse),(status=401,description="需要登录"),(status=403,description="需要绑定手机号"),(status=404,description="当前发布词条未引用该音频"),(status=501,description="音频存储未配置"),(status=503,description="音频服务不可用")))]
pub async fn audio(
    State(state): State<AppState>,
    _auth: AuthUser,
    ApiPath((id, asset_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<(HeaderMap, Json<AudioAssetUrlResponse>), AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let item = service::entries(&mut tx, &[id])
        .await?
        .pop()
        .ok_or_else(|| AppError::not_found("词条暂不可用"))?;
    if !item.audio.iter().any(|a| a.asset_id == asset_id) {
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
