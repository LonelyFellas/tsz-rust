use super::{dto::*, review};
use crate::lexicon::sentence_formatting::response as formatting_response;
use crate::{
    admin::AdminAuth,
    api::{ApiJson, ApiPath, ApiQuery},
    error::AppError,
    state::AppState,
};
use axum::{Json, extract::State};
use axum::{
    http::{HeaderMap, StatusCode},
    response::Response,
};
use uuid::Uuid;

#[utoipa::path(get,path="/api/v1/admin/wordlists",tag="wordlists",params(AdminWordlistQuery),security(("bearer_auth"=[])),
responses((status=200,description="词表管理",body=WordlistPage),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<AdminWordlistQuery>,
) -> Result<Json<WordlistPage>, AppError> {
    Ok(Json(review::admin_list(&state.pool, &auth, query).await?))
}

#[utoipa::path(get,path="/api/v1/admin/wordlists/{id}/review-requests",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),
responses((status=200,description="词表管理",body=WordlistReviews),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn reviews(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<WordlistReviews>, AppError> {
    Ok(Json(review::admin_reviews(&state.pool, &auth, id).await?))
}

#[utoipa::path(get,path="/api/v1/admin/wordlists/{id}/review-requests/{request_id}/items",tag="wordlists",params(("id"=Uuid,Path),("request_id"=Uuid,Path),WordlistQuery,("X-TSZ-Sentence-Formatting"=Option<String>,Header,description="v1 返回完整句子格式；缺省或未知值隐藏新增格式，并保护旧保存。",example="v1")),security(("bearer_auth"=[])),
responses((status=200,description="词表管理",body=WordlistItems),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn items(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    ApiPath((id, request_id)): ApiPath<(Uuid, Uuid)>,
    ApiQuery(query): ApiQuery<WordlistQuery>,
) -> Result<Response, AppError> {
    formatting_response(
        &headers,
        StatusCode::OK,
        review::admin_items(&state.pool, &auth, id, request_id, query).await?,
    )
}

#[utoipa::path(post,path="/api/v1/admin/wordlists/{id}/review-requests/{request_id}/decision",tag="wordlists",params(("id"=Uuid,Path),("request_id"=Uuid,Path)),security(("bearer_auth"=[])),request_body=WordlistDecision,
responses((status=200,description="词表管理",body=Wordlist),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn decision(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath((id, request_id)): ApiPath<(Uuid, Uuid)>,
    ApiJson(input): ApiJson<WordlistDecision>,
) -> Result<Json<Wordlist>, AppError> {
    Ok(Json(
        review::decision(&state.pool, &auth, id, request_id, input).await?,
    ))
}

#[utoipa::path(post,path="/api/v1/admin/wordlists/{id}/withdraw",tag="wordlists",params(("id"=Uuid,Path)),security(("bearer_auth"=[])),request_body=AdminWithdrawWordlist,
responses((status=200,description="词表管理",body=Wordlist),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn withdraw(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<AdminWithdrawWordlist>,
) -> Result<Json<Wordlist>, AppError> {
    Ok(Json(
        review::admin_withdraw(&state.pool, &auth, id, input).await?,
    ))
}

use super::management;
#[utoipa::path(get,path="/api/v1/admin/wordlists/management",tag="wordlists",security(("bearer_auth"=[])),params(WordlistManagementQuery),responses((status=200,description="词表管理",body=AdminWordlistPage),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn management_list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<WordlistManagementQuery>,
) -> Result<Json<AdminWordlistPage>, AppError> {
    Ok(Json(management::list(&state.pool, &auth, query).await?))
}
#[utoipa::path(get,path="/api/v1/admin/wordlists/{id}",tag="wordlists",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,description="词表管理",body=AdminWordlist),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn detail(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<AdminWordlist>, AppError> {
    Ok(Json(management::detail(&state.pool, &auth, id).await?))
}
#[utoipa::path(get,path="/api/v1/admin/wordlists/{id}/items",tag="wordlists",security(("bearer_auth"=[])),params(("id"=Uuid,Path),WordlistItemsQuery,("X-TSZ-Sentence-Formatting"=Option<String>,Header,description="v1 保留完整句子格式")),responses((status=200,description="词表管理",body=WordlistItems),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn current_items(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<WordlistItemsQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    formatting_response(
        &headers,
        StatusCode::OK,
        management::items(&state.pool, &auth, id, query).await?,
    )
}
#[utoipa::path(get,path="/api/v1/admin/wordlists/catalog",tag="wordlists",security(("bearer_auth"=[])),params(WordlistQuery),responses((status=200,description="词表管理",body=WordlistCatalog),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn catalog(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<WordlistQuery>,
) -> Result<Json<WordlistCatalog>, AppError> {
    Ok(Json(management::catalog(&state.pool, &auth, query).await?))
}
#[utoipa::path(post,path="/api/v1/admin/wordlists/{id}/visibility",tag="wordlists",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=WordlistVisibility,responses((status=200,description="词表管理",body=AdminWordlist),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn visibility(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<WordlistVisibility>,
) -> Result<Json<AdminWordlist>, AppError> {
    Ok(Json(
        management::visibility(&state.pool, &auth, id, input).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/admin/wordlists/{id}/recommendation",tag="wordlists",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=WordlistRecommendation,responses((status=200,description="词表管理",body=AdminWordlist),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn recommend(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<WordlistRecommendation>,
) -> Result<Json<AdminWordlist>, AppError> {
    Ok(Json(
        management::recommend(&state.pool, &auth, id, input).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/admin/wordlists/{id}/delete",tag="wordlists",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=WithdrawWordlist,responses((status=204,description="词表管理"),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn delete(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<WithdrawWordlist>,
) -> Result<StatusCode, AppError> {
    management::delete(&state.pool, &auth, id, input).await?;
    Ok(StatusCode::NO_CONTENT)
}
