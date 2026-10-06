use super::{dto::*, review};
use crate::{
    admin::AdminAuth,
    api::{ApiJson, ApiPath, ApiQuery},
    error::AppError,
    state::AppState,
};
use axum::{Json, extract::State};
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

#[utoipa::path(get,path="/api/v1/admin/wordlists/{id}/review-requests/{request_id}/items",tag="wordlists",params(("id"=Uuid,Path),("request_id"=Uuid,Path),WordlistQuery),security(("bearer_auth"=[])),
responses((status=200,description="词表管理",body=WordlistItems),(status=400,description="内容无效"),(status=401,description="会话无效"),(status=403,description="权限不足"),(status=404,description="不存在"),(status=409,description="版本或状态冲突")))]
pub async fn items(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath((id, request_id)): ApiPath<(Uuid, Uuid)>,
    ApiQuery(query): ApiQuery<WordlistQuery>,
) -> Result<Json<WordlistItems>, AppError> {
    Ok(Json(
        review::admin_items(&state.pool, &auth, id, request_id, query).await?,
    ))
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
