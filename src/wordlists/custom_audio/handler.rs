use super::dto::{AudioQuery, AudioState, AudioStatus, AudioStatuses};
use crate::{
    admin::AdminAuth,
    api::{ApiPath, ApiQuery},
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
    lexicon::audio_assets::dto::AudioAssetUrlResponse,
    platform::storage::ObjectKey,
    state::AppState,
    wordlists::{dto::Wordlist, review, service, service::Tx},
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
        .route("/api/v1/me/wordlists/{id}/custom-audio", get(owner_statuses))
        .route("/api/v1/wordlists/{id}/custom-audio", get(public_statuses))
        .route("/api/v1/admin/wordlists/{id}/custom-audio", get(admin_statuses))
        .route(
            "/api/v1/admin/wordlists/{id}/review-requests/{request_id}/custom-audio",
            get(review_statuses),
        )
        .route("/api/v1/me/wordlists/{id}/custom-audio/{item_id}/{locale}", get(owner_url))
        .route("/api/v1/wordlists/{id}/custom-audio/{item_id}/{locale}", get(public_url))
        .route("/api/v1/admin/wordlists/{id}/custom-audio/{item_id}/{locale}", get(admin_url))
        .route(
            "/api/v1/admin/wordlists/{id}/review-requests/{request_id}/custom-audio/{item_id}/{locale}",
            get(review_url),
        )
}

fn private_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    headers
}

#[derive(Clone, Copy)]
enum Access<'a> {
    Owner(&'a AuthUser),
    Public,
    Admin(&'a AdminAuth),
    Review(&'a AdminAuth, Uuid),
}

async fn lock_access(tx: &mut Tx<'_>, id: Uuid, access: Access<'_>) -> Result<Wordlist, AppError> {
    match access {
        Access::Owner(auth) => service::read_lock(tx, id, Some(auth)).await,
        Access::Public => service::read_lock(tx, id, None).await,
        Access::Admin(auth) | Access::Review(auth, _) => {
            review::admin_list_lock(tx, auth, id, "wordlists.access", false).await
        }
    }
}

async fn check_before_return(
    tx: &mut Tx<'_>,
    list: &Wordlist,
    access: Access<'_>,
) -> Result<(), AppError> {
    match access {
        Access::Owner(auth) => {
            crate::account_deletion::ensure_not_effective_in(tx, auth.subject).await
        }
        Access::Public => service::eligible_owner(tx, list.owner_user_id).await,
        Access::Admin(_) | Access::Review(_, _) => Ok(()),
    }
}

fn item_ids(query: AudioQuery) -> Result<Vec<Uuid>, AppError> {
    let ids: Vec<_> = query.item_ids.split(',').collect();
    if ids.len() > 100 {
        return Err(AppError::bad_request(
            ErrorCode::InvalidQuery,
            "每次最多查询 100 项",
        ));
    }
    ids.into_iter()
        .map(|id| {
            Uuid::parse_str(id.trim()).map_err(|_| {
                AppError::bad_request(ErrorCode::InvalidQuery, "item_ids 须为逗号分隔的 UUID")
            })
        })
        .collect()
}

#[derive(sqlx::FromRow)]
struct ReferencedJob {
    item_id: Uuid,
    job_id: Uuid,
    word: String,
    status: AudioState,
    test_run: Option<String>,
}

// Both status and signing use these references. A current flag alone must never
// expose audio after a text change; a review must keep its exact submitted job.
async fn referenced_jobs(
    tx: &mut Tx<'_>,
    id: Uuid,
    ids: &[Uuid],
    access: Access<'_>,
) -> Result<Vec<ReferencedJob>, AppError> {
    if let Access::Review(_, request) = access {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM wordlist_review_requests WHERE id=$1 AND wordlist_id=$2)",
        )
        .bind(request)
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)?;
        if !exists {
            return Err(AppError::not_found("review not found"));
        }
        sqlx::query_as(
            "SELECT j.item_id,j.id AS job_id,j.word,j.state AS status,j.test_run
             FROM wordlist_audio_review_refs r
             JOIN wordlist_review_requests q ON q.id=r.review_id
             JOIN wordlist_audio_jobs j ON j.id=r.job_id AND j.item_id=r.item_id AND j.wordlist_id=q.wordlist_id
             WHERE q.wordlist_id=$1 AND q.id=$2 AND r.item_id=ANY($3)
               AND EXISTS(SELECT 1 FROM jsonb_array_elements(q.item_snapshots) AS snapshot
                          WHERE snapshot->>'item_id'=r.item_id::text
                            AND btrim(snapshot->'custom_word'->>'word')=j.word)
             ORDER BY j.item_id",
        )
        .bind(id)
        .bind(request)
        .bind(ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)
    } else {
        sqlx::query_as(
            "SELECT j.item_id,j.id AS job_id,j.word,j.state AS status,j.test_run
             FROM wordlist_audio_jobs j
             JOIN wordlist_items i ON i.wordlist_id=j.wordlist_id AND i.item_id=j.item_id
             JOIN wordlists w ON w.id=j.wordlist_id
             WHERE j.wordlist_id=$1 AND j.item_id=ANY($2) AND j.is_current
               AND btrim(i.custom_word->>'word')=j.word AND w.deleted_at IS NULL
             ORDER BY i.position",
        )
        .bind(id)
        .bind(ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)
    }
}

async fn statuses(
    state: &AppState,
    id: Uuid,
    query: AudioQuery,
    access: Access<'_>,
) -> Result<(HeaderMap, Json<AudioStatuses>), AppError> {
    let ids = item_ids(query)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let list = lock_access(&mut tx, id, access).await?;
    let items = referenced_jobs(&mut tx, id, &ids, access)
        .await?
        .into_iter()
        .filter(|job| !matches!(access, Access::Public) || matches!(job.status, AudioState::Ready))
        .map(|job| AudioStatus {
            item_id: job.item_id,
            job_id: job.job_id,
            word: job.word,
            locales: if matches!(job.status, AudioState::Ready) {
                vec!["en-GB".into(), "en-US".into()]
            } else {
                vec![]
            },
            status: job.status,
        })
        .collect();
    check_before_return(&mut tx, &list, access).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok((private_headers(), Json(AudioStatuses { items })))
}

async fn signed_url(
    state: &AppState,
    id: Uuid,
    item: Uuid,
    locale: &str,
    access: Access<'_>,
) -> Result<(HeaderMap, Json<AudioAssetUrlResponse>), AppError> {
    if !matches!(locale, "en-GB" | "en-US") {
        return Err(AppError::bad_request(
            ErrorCode::InvalidPathParameter,
            "locale 须为 en-GB 或 en-US",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let list = lock_access(&mut tx, id, access).await?;
    let job = referenced_jobs(&mut tx, id, &[item], access)
        .await?
        .into_iter()
        .find(|job| matches!(job.status, AudioState::Ready))
        .ok_or_else(|| AppError::not_found("语音暂不可用"))?;
    let key: String = sqlx::query_scalar(
        "SELECT o.object_key FROM wordlist_audio_objects o
         JOIN wordlist_audio_jobs j ON j.id=o.job_id AND j.lease_token=o.attempt_token
         WHERE j.id=$1 AND j.state='ready' AND o.locale=$2 AND o.uploaded AND o.deleted_at IS NULL",
    )
    .bind(job.job_id)
    .bind(locale)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("语音暂不可用"))?;
    let store = super::storage(state, job.test_run.as_deref())?;
    let signed = store
        .presign_read(&ObjectKey::parse(key).map_err(AppError::internal)?)
        .await
        .map_err(|_| {
            AppError::unavailable(ErrorCode::SpeechStorageUnavailable, "语音服务暂不可用")
        })?;
    check_before_return(&mut tx, &list, access).await?;
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

#[utoipa::path(get,path="/api/v1/me/wordlists/{id}/custom-audio",tag="wordlists",params(("id"=Uuid,Path),AudioQuery),security(("bearer_auth"=[])),
responses((status=200,description="本人当前自定义词汇语音状态",body=AudioStatuses),(status=400,description="参数无效"),(status=401,description="会话无效"),(status=403,description="需要绑定手机号"),(status=404,description="不存在或不可访问")))]
pub async fn owner_statuses(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<AudioQuery>,
) -> Result<(HeaderMap, Json<AudioStatuses>), AppError> {
    statuses(&state, id, query, Access::Owner(&auth)).await
}

#[utoipa::path(get,path="/api/v1/wordlists/{id}/custom-audio",tag="wordlists",params(("id"=Uuid,Path),AudioQuery),
responses((status=200,description="公开词表已就绪语音",body=AudioStatuses),(status=400,description="参数无效"),(status=404,description="不存在或不可访问")))]
pub async fn public_statuses(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<AudioQuery>,
) -> Result<(HeaderMap, Json<AudioStatuses>), AppError> {
    statuses(&state, id, query, Access::Public).await
}

#[utoipa::path(get,path="/api/v1/admin/wordlists/{id}/custom-audio",tag="wordlists",params(("id"=Uuid,Path),AudioQuery),security(("bearer_auth"=[])),
responses((status=200,description="当前词表语音状态，不含资金信息",body=AudioStatuses),(status=400,description="参数无效"),(status=401,description="会话无效"),(status=403,description="需要 wordlists.access 权限"),(status=404,description="不存在或不可访问")))]
pub async fn admin_statuses(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<AudioQuery>,
) -> Result<(HeaderMap, Json<AudioStatuses>), AppError> {
    statuses(&state, id, query, Access::Admin(&auth)).await
}

#[utoipa::path(get,path="/api/v1/admin/wordlists/{id}/review-requests/{request_id}/custom-audio",tag="wordlists",params(("id"=Uuid,Path),("request_id"=Uuid,Path),AudioQuery),security(("bearer_auth"=[])),
responses((status=200,description="审核快照固定任务的语音状态",body=AudioStatuses),(status=400,description="参数无效"),(status=401,description="会话无效"),(status=403,description="需要 wordlists.access 权限"),(status=404,description="不存在或不可访问")))]
pub async fn review_statuses(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath((id, request)): ApiPath<(Uuid, Uuid)>,
    ApiQuery(query): ApiQuery<AudioQuery>,
) -> Result<(HeaderMap, Json<AudioStatuses>), AppError> {
    statuses(&state, id, query, Access::Review(&auth, request)).await
}

#[utoipa::path(get,path="/api/v1/me/wordlists/{id}/custom-audio/{item_id}/{locale}",tag="wordlists",params(("id"=Uuid,Path),("item_id"=Uuid,Path),("locale"=String,Path,description="en-GB 或 en-US")),security(("bearer_auth"=[])),
responses((status=200,description="已就绪语音的短期只读 URL",body=AudioAssetUrlResponse),(status=400,description="参数无效"),(status=401,description="会话无效"),(status=403,description="需要绑定手机号"),(status=404,description="语音不存在或不可访问"),(status=503,description="语音存储暂不可用")))]
pub async fn owner_url(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath((id, item, locale)): ApiPath<(Uuid, Uuid, String)>,
) -> Result<(HeaderMap, Json<AudioAssetUrlResponse>), AppError> {
    signed_url(&state, id, item, &locale, Access::Owner(&auth)).await
}

#[utoipa::path(get,path="/api/v1/wordlists/{id}/custom-audio/{item_id}/{locale}",tag="wordlists",params(("id"=Uuid,Path),("item_id"=Uuid,Path),("locale"=String,Path,description="en-GB 或 en-US")),
responses((status=200,description="公开词表语音的短期只读 URL",body=AudioAssetUrlResponse),(status=400,description="参数无效"),(status=404,description="语音不存在或不可访问"),(status=503,description="语音存储暂不可用")))]
pub async fn public_url(
    State(state): State<AppState>,
    ApiPath((id, item, locale)): ApiPath<(Uuid, Uuid, String)>,
) -> Result<(HeaderMap, Json<AudioAssetUrlResponse>), AppError> {
    signed_url(&state, id, item, &locale, Access::Public).await
}

#[utoipa::path(get,path="/api/v1/admin/wordlists/{id}/custom-audio/{item_id}/{locale}",tag="wordlists",params(("id"=Uuid,Path),("item_id"=Uuid,Path),("locale"=String,Path,description="en-GB 或 en-US")),security(("bearer_auth"=[])),
responses((status=200,description="当前词表语音的短期只读 URL",body=AudioAssetUrlResponse),(status=400,description="参数无效"),(status=401,description="会话无效"),(status=403,description="需要 wordlists.access 权限"),(status=404,description="语音不存在或不可访问"),(status=503,description="语音存储暂不可用")))]
pub async fn admin_url(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath((id, item, locale)): ApiPath<(Uuid, Uuid, String)>,
) -> Result<(HeaderMap, Json<AudioAssetUrlResponse>), AppError> {
    signed_url(&state, id, item, &locale, Access::Admin(&auth)).await
}

#[utoipa::path(get,path="/api/v1/admin/wordlists/{id}/review-requests/{request_id}/custom-audio/{item_id}/{locale}",tag="wordlists",params(("id"=Uuid,Path),("request_id"=Uuid,Path),("item_id"=Uuid,Path),("locale"=String,Path,description="en-GB 或 en-US")),security(("bearer_auth"=[])),
responses((status=200,description="审核快照语音的短期只读 URL",body=AudioAssetUrlResponse),(status=400,description="参数无效"),(status=401,description="会话无效"),(status=403,description="需要 wordlists.access 权限"),(status=404,description="语音不存在或不可访问"),(status=503,description="语音存储暂不可用")))]
pub async fn review_url(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath((id, request, item, locale)): ApiPath<(Uuid, Uuid, Uuid, String)>,
) -> Result<(HeaderMap, Json<AudioAssetUrlResponse>), AppError> {
    signed_url(&state, id, item, &locale, Access::Review(&auth, request)).await
}
