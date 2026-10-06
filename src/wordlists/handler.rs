use super::{dto::*, review, service};
use crate::{
    api::{ApiJson, ApiPath, ApiQuery},
    auth::extract::AuthUser,
    error::AppError,
    state::AppState,
};
use axum::{Json, Router, extract::State, routing::get};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/me/wordlist-tips/{event_id}",
            get(super::tips::receipt),
        )
        .route(
            "/api/v1/wordlists/{id}/tips",
            axum::routing::post(super::tips::create),
        )
        .route("/api/v1/me/wordlist-tips", get(super::tips::history))
        .route("/api/v1/wordlists", get(public_list))
        .route("/api/v1/wordlists/catalog", get(catalog))
        .route("/api/v1/wordlists/{id}", get(public_detail))
        .route("/api/v1/wordlists/{id}/items", get(public_items))
        .route("/api/v1/me/wordlists", get(my_list).post(create))
        .route("/api/v1/me/wordlists/{id}", get(my_detail).put(update))
        .route("/api/v1/me/wordlists/{id}/items", get(my_items))
        .route("/api/v1/me/wordlists/{id}/edit", get(edit))
        .route(
            "/api/v1/me/wordlists/{id}/review-requests",
            get(reviews).post(submit),
        )
        .route(
            "/api/v1/me/wordlists/{id}/withdraw",
            axum::routing::post(withdraw),
        )
}

#[utoipa::path(get,path="/api/v1/wordlists",tag="wordlists",params(WordlistQuery),
responses((status=200,description="词表分页",body=WordlistPage),(status=400,description="参数无效"),(status=401,description="会话无效")))]
pub async fn public_list(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<WordlistQuery>,
) -> Result<Json<WordlistPage>, AppError> {
    Ok(Json(service::list(&state.pool, None, query).await?))
}

#[utoipa::path(get,path="/api/v1/me/wordlists",tag="wordlists",params(WordlistQuery),security(("bearer_auth"=[])),
responses((status=403,description="需要先绑定手机号"),(status=200,description="词表分页",body=WordlistPage),(status=400,description="参数无效"),(status=401,description="会话无效")))]
pub async fn my_list(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(query): ApiQuery<WordlistQuery>,
) -> Result<Json<WordlistPage>, AppError> {
    Ok(Json(service::list(&state.pool, Some(&auth), query).await?))
}

#[utoipa::path(get,path="/api/v1/wordlists/{id}",tag="wordlists",params(("id"=Uuid,Path)),
responses((status=200,description="词表元数据",body=Wordlist),(status=404,description="不存在或不可访问"),(status=401,description="会话无效")))]
pub async fn public_detail(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<Wordlist>, AppError> {
    Ok(Json(service::read(&state.pool, id, None).await?))
}

#[utoipa::path(get,path="/api/v1/me/wordlists/{id}",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),
responses((status=403,description="需要先绑定手机号"),(status=200,description="词表元数据",body=Wordlist),(status=404,description="不存在或不可访问"),(status=401,description="会话无效")))]
pub async fn my_detail(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<Wordlist>, AppError> {
    Ok(Json(service::read(&state.pool, id, Some(&auth)).await?))
}

#[utoipa::path(get,path="/api/v1/wordlists/{id}/items",tag="wordlists",params(("id"=Uuid,Path),WordlistQuery),
responses((status=200,description="当前发布内容；不可用条目不返回历史内容",body=WordlistItems),(status=400,description="参数无效"),(status=404,description="不可访问"),(status=401,description="会话无效")))]
pub async fn public_items(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<WordlistQuery>,
) -> Result<Json<WordlistItems>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let list = service::read_lock(&mut tx, id, None).await?;
    let (rows, pagination) = service::item_rows(&mut tx, id, &query).await?;
    let mut entries = service::read_entries(
        &mut tx,
        &rows.iter().map(|r| r.entry_id).collect::<Vec<_>>(),
    )
    .await?;
    let items = rows
        .into_iter()
        .map(|r| WordlistItem {
            entry_id: r.entry_id,
            position: r.position,
            entry: entries.remove(&r.entry_id),
        })
        .collect();
    service::eligible_owner(&mut tx, list.owner_user_id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(WordlistItems {
        items,
        revision: list.revision,
        pagination,
    }))
}

#[utoipa::path(get,path="/api/v1/me/wordlists/{id}/items",tag="wordlists",params(("id"=Uuid,Path),WordlistQuery),security(("bearer_auth"=[])),
responses((status=403,description="需要先绑定手机号"),(status=200,description="当前发布内容；不可用条目不返回历史内容",body=MyWordlistItems),(status=400,description="参数无效"),(status=404,description="不可访问"),(status=401,description="会话无效")))]
pub async fn my_items(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<WordlistQuery>,
) -> Result<Json<MyWordlistItems>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let list = service::read_lock(&mut tx, id, Some(&auth)).await?;
    let (rows, pagination) = service::item_rows(&mut tx, id, &query).await?;
    let mut entries = service::read_entries(
        &mut tx,
        &rows.iter().map(|r| r.entry_id).collect::<Vec<_>>(),
    )
    .await?;
    let items = rows
        .into_iter()
        .map(|r| MyWordlistItem {
            entry_id: r.entry_id,
            position: r.position,
            entry: entries.remove(&r.entry_id),
            private_note: r.private_note,
            note_revision: r.note_revision,
        })
        .collect();
    service::eligible_owner(&mut tx, list.owner_user_id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(MyWordlistItems {
        items,
        revision: list.revision,
        pagination,
    }))
}

#[utoipa::path(post,path="/api/v1/me/wordlists",tag="wordlists",security(("bearer_auth"=[])),request_body=CreateWordlist,
responses((status=403,description="需要先绑定手机号"),(status=200,description="创建或重放",body=Wordlist),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=409,description="请求键冲突")))]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(input): ApiJson<CreateWordlist>,
) -> Result<Json<Wordlist>, AppError> {
    Ok(Json(service::create(&state.pool, &auth, input).await?))
}
#[utoipa::path(put,path="/api/v1/me/wordlists/{id}",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),request_body=UpdateWordlist,
responses((status=403,description="需要先绑定手机号"),(status=200,description="原子保存内容和备注",body=Wordlist),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=404,description="不可访问"),(status=409,description="版本或状态冲突")))]
pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<UpdateWordlist>,
) -> Result<Json<Wordlist>, AppError> {
    Ok(Json(service::update(&state.pool, &auth, id, input).await?))
}
#[utoipa::path(get,path="/api/v1/me/wordlists/{id}/edit",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),
responses((status=403,description="需要先绑定手机号"),(status=200,description="一致版本下的完整有序ID；词义和备注独立分页",body=WordlistEditSnapshot),(status=401,description="会话无效"),(status=404,description="不可访问")))]
pub async fn edit(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<WordlistEditSnapshot>, AppError> {
    Ok(Json(service::edit_snapshot(&state.pool, id, &auth).await?))
}
#[utoipa::path(get,path="/api/v1/wordlists/catalog",tag="wordlists",params(WordlistQuery),security(("bearer_auth"=[])),
responses((status=403,description="需要先绑定手机号"),(status=200,description="仅已发布且未归档的词条候选",body=WordlistCatalog),(status=400,description="参数无效"),(status=401,description="会话无效")))]
pub async fn catalog(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(query): ApiQuery<WordlistQuery>,
) -> Result<Json<WordlistCatalog>, AppError> {
    Ok(Json(service::catalog(&state.pool, &auth, query).await?))
}

#[utoipa::path(post,path="/api/v1/me/wordlists/{id}/review-requests",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),request_body=SubmitWordlist,
responses((status=403,description="需要先绑定手机号"),(status=200,description="词表审核状态",body=WordlistReview),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=404,description="不可访问"),(status=409,description="版本或状态冲突")))]
pub async fn submit(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<SubmitWordlist>,
) -> Result<Json<WordlistReview>, AppError> {
    Ok(Json(review::submit(&state.pool, &auth, id, input).await?))
}

#[utoipa::path(post,path="/api/v1/me/wordlists/{id}/withdraw",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),request_body=WithdrawWordlist,
responses((status=403,description="需要先绑定手机号"),(status=200,description="词表审核状态",body=Wordlist),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=404,description="不可访问"),(status=409,description="版本或状态冲突")))]
pub async fn withdraw(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<WithdrawWordlist>,
) -> Result<Json<Wordlist>, AppError> {
    Ok(Json(review::withdraw(&state.pool, &auth, id, input).await?))
}

#[utoipa::path(get,path="/api/v1/me/wordlists/{id}/review-requests",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),
responses((status=403,description="需要先绑定手机号"),(status=200,description="词表审核状态",body=WordlistReviews),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=404,description="不可访问"),(status=409,description="版本或状态冲突")))]
pub async fn reviews(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<WordlistReviews>, AppError> {
    Ok(Json(review::reviews(&state.pool, &auth, id).await?))
}
