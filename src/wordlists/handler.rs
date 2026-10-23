use super::annotations::response as formatting_response;
use super::{dto::*, review, service};
use crate::{
    api::{ApiJson, ApiPath, ApiQuery},
    auth::extract::AuthUser,
    error::AppError,
    state::AppState,
};
use axum::{Json, Router, extract::State, routing::get};
use axum::{
    http::{HeaderMap, StatusCode},
    response::Response,
};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(super::library::router())
        .merge(super::custom_audio::handler::router())
        .route(
            "/api/v1/wordlists/interactions",
            get(super::interactions::stats),
        )
        .route(
            "/api/v1/me/wordlist-interactions",
            get(super::interactions::states),
        )
        .route(
            "/api/v1/me/wordlist-favorites",
            get(super::interactions::favorites),
        )
        .route(
            "/api/v1/me/wordlists/{id}/interactions",
            axum::routing::put(super::interactions::set),
        )
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
        .route("/api/v1/wordlists/recommended", get(recommended))
        .route(
            "/api/v1/me/wordlists/{id}/publish",
            axum::routing::post(publish),
        )
        .route("/api/v1/wordlists/catalog", get(catalog))
        .route("/api/v1/wordlists/suggestions", get(suggestions))
        .route(
            "/api/v1/wordlists/import-matches",
            axum::routing::post(import_matches),
        )
        .route("/api/v1/wordlists/{id}", get(public_detail))
        .route("/api/v1/wordlists/{id}/items", get(public_items))
        .route(
            "/api/v1/wordlists/{id}/availability",
            get(public_availability),
        )
        .route("/api/v1/me/wordlists", get(my_list).post(create))
        .route("/api/v1/me/wordlists/{id}", get(my_detail).put(update))
        .route("/api/v1/me/wordlists/{id}/items", get(my_items))
        .route(
            "/api/v1/me/wordlists/{id}/availability",
            get(my_availability),
        )
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

#[utoipa::path(get,path="/api/v1/wordlists/{id}/availability",tag="wordlists",params(("id"=Uuid,Path),("X-TSZ-Wordlist-Custom-Items"=Option<String>,Header,description="v1 返回稳定项标识、自定义词汇和计数；旧客户端读取自定义内容返回 409",example="v1")),
responses((status=409,description="客户端不支持此词表的自定义内容，请刷新页面"),(status=200,description="整张词表的当前可用数量，不受条目分页和搜索影响",body=WordlistAvailability),(status=404,description="不存在或不可访问")))]
pub async fn public_availability(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    formatting_response(
        &headers,
        StatusCode::OK,
        service::availability(&state.pool, id, None).await?,
    )
}

#[utoipa::path(get,path="/api/v1/me/wordlists/{id}/availability",tag="wordlists",params(("id"=Uuid,Path),("X-TSZ-Wordlist-Custom-Items"=Option<String>,Header,description="v1 返回稳定项标识、自定义词汇和计数；旧客户端读取自定义内容返回 409",example="v1")),security(("bearer_auth"=[])),
responses((status=409,description="客户端不支持此词表的自定义内容，请刷新页面"),(status=200,description="本人整张词表的当前可用数量",body=WordlistAvailability),(status=401,description="会话无效"),(status=403,description="需要先绑定手机号"),(status=404,description="不存在或不可访问")))]
pub async fn my_availability(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    formatting_response(
        &headers,
        StatusCode::OK,
        service::availability(&state.pool, id, Some(&auth)).await?,
    )
}

#[utoipa::path(get,path="/api/v1/wordlists/{id}/items",tag="wordlists",params(("id"=Uuid,Path),("X-TSZ-Wordlist-Custom-Items"=Option<String>,Header,description="v1 返回稳定项标识、自定义词汇和计数；旧客户端读取自定义内容返回 409",example="v1"),WordlistItemsQuery,("X-TSZ-Wordlist-Annotations"=Option<String>,Header,description="v1 返回作者注释；缺省或未知值保持旧客户端响应结构",example="v1"),("X-TSZ-Sentence-Formatting"=Option<String>,Header,description="v1 返回完整句子格式；缺省或未知值隐藏新增格式，并保护旧保存。",example="v1")),
responses((status=409,description="客户端不支持此词表的自定义内容，请刷新页面"),(status=200,description="当前发布内容；不可用条目不返回历史内容",body=WordlistItems),(status=400,description="参数无效"),(status=404,description="不可访问"),(status=401,description="会话无效")))]
pub async fn public_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<WordlistItemsQuery>,
) -> Result<Response, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let list = service::read_lock(&mut tx, id, None).await?;
    let (rows, pagination) = service::item_rows(&mut tx, id, &query).await?;
    let entries = service::read_entries_view(
        &mut tx,
        &rows.iter().filter_map(|r| r.entry_id).collect::<Vec<_>>(),
        query.view,
    )
    .await?;
    let items = service::public_items(rows, entries);
    service::eligible_owner(&mut tx, list.owner_user_id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    formatting_response(
        &headers,
        StatusCode::OK,
        WordlistItems {
            items,
            revision: list.revision,
            pagination,
        },
    )
}

#[utoipa::path(get,path="/api/v1/me/wordlists/{id}/items",tag="wordlists",params(("id"=Uuid,Path),("X-TSZ-Wordlist-Custom-Items"=Option<String>,Header,description="v1 返回稳定项标识、自定义词汇和计数；旧客户端读取自定义内容返回 409",example="v1"),WordlistItemsQuery,("X-TSZ-Wordlist-Entry-Reasons"=Option<String>,Header,description="v1 仅向作者返回当前不可用原因；缺省或未知值不返回新增字段",example="v1"),("X-TSZ-Wordlist-Entry-Labels"=Option<String>,Header,description="v1 为不可用引用返回最后发布词面，仅作者可见；缺省或未知值不返回新增字段",example="v1"),("X-TSZ-Wordlist-Annotations"=Option<String>,Header,description="v1 返回作者注释；缺省或未知值保持旧客户端响应结构",example="v1"),("X-TSZ-Sentence-Formatting"=Option<String>,Header,description="v1 返回完整句子格式；缺省或未知值隐藏新增格式，并保护旧保存。",example="v1")),security(("bearer_auth"=[])),
responses((status=409,description="客户端不支持此词表的自定义内容，请刷新页面"),(status=403,description="需要先绑定手机号"),(status=200,description="当前发布内容；不可用条目可按请求头返回词面，不返回历史释义",body=MyWordlistItems),(status=400,description="参数无效"),(status=404,description="不可访问"),(status=401,description="会话无效")))]
pub async fn my_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<WordlistItemsQuery>,
) -> Result<Response, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let list = service::read_lock(&mut tx, id, Some(&auth)).await?;
    let (rows, pagination) = service::item_rows(&mut tx, id, &query).await?;
    let mut entries = service::read_entries_view(
        &mut tx,
        &rows.iter().filter_map(|r| r.entry_id).collect::<Vec<_>>(),
        query.view,
    )
    .await?;
    let unavailable: Vec<_> = rows
        .iter()
        .filter_map(|r| r.entry_id.filter(|id| !entries.contains_key(id)))
        .collect();
    let mut reasons = if !unavailable.is_empty()
        && headers
            .get("X-TSZ-Wordlist-Entry-Reasons")
            .is_some_and(|value| value == "v1")
    {
        service::unavailable_reasons(&mut tx, &unavailable).await?
    } else {
        std::collections::HashMap::new()
    };
    let mut labels = if headers
        .get("X-TSZ-Wordlist-Entry-Labels")
        .is_some_and(|value| value == "v1")
    {
        let labels: Vec<(Uuid, String)> = sqlx::query_as("SELECT DISTINCT ON (entry_id) entry_id, snapshot#>>'{presentation,label}' FROM lexicon.entry_publications WHERE entry_id=ANY($1) AND NULLIF(snapshot#>>'{presentation,label}', '') IS NOT NULL ORDER BY entry_id, publication_number DESC")
            .bind(&unavailable).fetch_all(&mut *tx).await.map_err(AppError::internal)?;
        labels
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
    } else {
        std::collections::HashMap::new()
    };
    let items = rows
        .into_iter()
        .map(|r| MyWordlistItem {
            entry_id: r.entry_id,
            item_id: Some(r.item_id),
            custom_word: r.custom_word.map(|v| v.0),
            position: r.position,
            entry_label: r.entry_id.and_then(|id| labels.remove(&id)),
            unavailable_reason: r.entry_id.and_then(|id| reasons.remove(&id)),
            author_note: Some(r.author_note),
            entry: r.entry_id.and_then(|id| entries.remove(&id)),
            private_note: r.private_note,
            note_revision: r.note_revision,
        })
        .collect();
    service::eligible_owner(&mut tx, list.owner_user_id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    let mut response = formatting_response(
        &headers,
        StatusCode::OK,
        MyWordlistItems {
            items,
            revision: list.revision,
            pagination,
        },
    )?;
    response.headers_mut().insert(
        axum::http::header::VARY,
        axum::http::HeaderValue::from_static(
            "x-tsz-sentence-formatting, x-tsz-wordlist-annotations, x-tsz-wordlist-entry-labels, x-tsz-wordlist-entry-reasons, x-tsz-wordlist-custom-items",
        ),
    );
    Ok(response)
}

#[utoipa::path(post,path="/api/v1/me/wordlists",tag="wordlists",security(("bearer_auth"=[])),request_body=CreateWordlist,
responses((status=403,description="需要先绑定手机号"),(status=200,description="创建或重放",body=Wordlist),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=409,description="请求键冲突")))]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    ApiJson(input): ApiJson<CreateWordlist>,
) -> Result<Json<Wordlist>, AppError> {
    Ok(Json(service::create(&state, &auth, input, &headers).await?))
}
#[utoipa::path(put,path="/api/v1/me/wordlists/{id}",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),request_body=UpdateWordlist,
responses((status=403,description="需要先绑定手机号"),(status=200,description="原子保存内容和备注",body=Wordlist),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=404,description="不可访问"),(status=409,description="版本或状态冲突")))]
pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<UpdateWordlist>,
) -> Result<Json<Wordlist>, AppError> {
    Ok(Json(
        service::update(&state, &auth, id, input, &headers).await?,
    ))
}
#[utoipa::path(get,path="/api/v1/me/wordlists/{id}/edit",tag="wordlists",params(("id"=Uuid,Path),("X-TSZ-Wordlist-Custom-Items"=Option<String>,Header,description="v1 返回稳定项标识、自定义词汇和计数；旧客户端读取自定义内容返回 409",example="v1")),security(("bearer_auth"=[])),
responses((status=409,description="客户端不支持此词表的自定义内容，请刷新页面"),(status=403,description="需要先绑定手机号"),(status=200,description="一致版本下的完整有序ID；词义和备注独立分页",body=WordlistEditSnapshot),(status=401,description="会话无效"),(status=404,description="不可访问")))]
pub async fn edit(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    formatting_response(
        &headers,
        StatusCode::OK,
        service::edit_snapshot(&state.pool, id, &auth).await?,
    )
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

#[utoipa::path(get,path="/api/v1/wordlists/recommended",tag="wordlists",params(WordlistQuery),responses((status=200,description="系统推荐公开词表",body=WordlistPage),(status=400,description="查询无效")))]
pub async fn recommended(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<WordlistQuery>,
) -> Result<Json<WordlistPage>, AppError> {
    Ok(Json(
        super::management::recommended(&state.pool, query).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/me/wordlists/{id}/publish",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),request_body=SubmitWordlist,responses((status=200,description="提交公开申请，等待管理员人工审核",body=WordlistReview),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="需要绑定手机号"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn publish(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<SubmitWordlist>,
) -> Result<Json<WordlistReview>, AppError> {
    Ok(Json(review::submit(&state.pool, &auth, id, input).await?))
}

#[utoipa::path(get,path="/api/v1/wordlists/suggestions",tag="wordlists",params(WordlistSuggestionQuery),security(("bearer_auth"=[])),responses((status=200,description="最多三个词条候选，无总数和分页",body=WordlistSuggestions),(status=400,description="查询无效"),(status=401,description="会话无效"),(status=403,description="需要绑定手机号")))]
pub async fn suggestions(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(query): ApiQuery<WordlistSuggestionQuery>,
) -> Result<Json<WordlistSuggestions>, AppError> {
    Ok(Json(
        super::suggestions::for_user(&state.pool, &auth, query).await?,
    ))
}

#[utoipa::path(post,path="/api/v1/wordlists/import-matches",tag="wordlists",request_body=WordlistImportMatchRequest,security(("bearer_auth"=[])),responses((status=200,description="按输入顺序返回精确匹配，不写入词表",body=WordlistImportMatches),(status=400,description="每批 1–100 个词汇，每词 1–200 字"),(status=401,description="会话无效"),(status=403,description="需要绑定手机号")))]
pub async fn import_matches(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(input): ApiJson<WordlistImportMatchRequest>,
) -> Result<Json<WordlistImportMatches>, AppError> {
    Ok(Json(
        super::suggestions::match_import(&state.pool, &auth, input).await?,
    ))
}
