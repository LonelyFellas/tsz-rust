use super::{dto::*, queries, review};
use crate::{
    api::{ApiJson, ApiPath, ApiQuery},
    auth::extract::AuthUser,
    error::AppError,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, patch, post},
};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/me/memory/overview", get(overview))
        .route("/api/v1/me/memory/enrollment", post(enrollment))
        .route("/api/v1/me/memory/items", get(items))
        .route("/api/v1/me/memory/items/{id}", patch(pause))
        .route("/api/v1/me/review-sessions", post(start))
        .route("/api/v1/me/review-sessions/{id}", get(detail))
        .route("/api/v1/me/review-sessions/{id}/answers", post(answer))
        .route(
            "/api/v1/me/review-sessions/{id}/items/{item_id}/reveal",
            post(reveal),
        )
        .layer(axum::middleware::from_fn(private_response))
}
async fn private_response(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response
}
#[utoipa::path(get,path="/api/v1/me/memory/overview",tag="learning_memory",security(("bearer_auth"=[])),responses((status=200,body=MemoryOverview),(status=401,description="会话失效"),(status=403,description="需要学生资格")))]
pub async fn overview(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<MemoryOverview>, AppError> {
    Ok(Json(queries::overview(&state, &auth).await?))
}
#[utoipa::path(post,path="/api/v1/me/memory/enrollment",tag="learning_memory",security(("bearer_auth"=[])),request_body=MemoryEnrollmentInput,responses((status=200,body=MemoryOverview),(status=404,description="功能尚未开放")))]
pub async fn enrollment(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(input): ApiJson<MemoryEnrollmentInput>,
) -> Result<Json<MemoryOverview>, AppError> {
    Ok(Json(queries::enrollment(&state, &auth, input).await?))
}
#[utoipa::path(get,path="/api/v1/me/memory/items",tag="learning_memory",security(("bearer_auth"=[])),params(MemoryQuery),responses((status=200,body=MemoryPage),(status=400,description="参数无效"),(status=404,description="功能尚未开放")))]
pub async fn items(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(q): ApiQuery<MemoryQuery>,
) -> Result<Json<MemoryPage>, AppError> {
    Ok(Json(queries::items(&state, &auth, q).await?))
}
#[utoipa::path(patch,path="/api/v1/me/memory/items/{id}",tag="learning_memory",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=MemoryPauseInput,responses((status=200,body=MemoryItem),(status=409,description="版本或归属冲突")))]
pub async fn pause(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<MemoryPauseInput>,
) -> Result<Json<MemoryItem>, AppError> {
    Ok(Json(queries::pause(&state, &auth, id, input).await?))
}
#[utoipa::path(post,path="/api/v1/me/review-sessions",tag="learning_memory",security(("bearer_auth"=[])),request_body=StartReview,responses((status=200,body=ReviewSession),(status=409,description="未开启或暂无可复习内容")))]
pub async fn start(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(input): ApiJson<StartReview>,
) -> Result<Json<ReviewSession>, AppError> {
    Ok(Json(review::start(&state, &auth, input).await?))
}
#[utoipa::path(get,path="/api/v1/me/review-sessions/{id}",tag="learning_memory",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,body=ReviewSession),(status=404,description="复习不存在")))]
pub async fn detail(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<ReviewSession>, AppError> {
    Ok(Json(review::get(&state, &auth, id).await?))
}
#[utoipa::path(post,path="/api/v1/me/review-sessions/{id}/answers",tag="learning_memory",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=ReviewAnswerInput,responses((status=200,body=ReviewReceipt),(status=409,description="请求、题目或版本冲突")))]
pub async fn answer(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<ReviewAnswerInput>,
) -> Result<Json<ReviewReceipt>, AppError> {
    Ok(Json(review::answer(&state, &auth, id, input).await?))
}
#[utoipa::path(post,path="/api/v1/me/review-sessions/{id}/items/{item_id}/reveal",tag="learning_memory",security(("bearer_auth"=[])),params(("id"=Uuid,Path),("item_id"=Uuid,Path)),responses((status=200,body=ReviewSession),(status=409,description="题目已失效")))]
pub async fn reveal(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath((id, item)): ApiPath<(Uuid, Uuid)>,
) -> Result<Json<ReviewSession>, AppError> {
    Ok(Json(review::reveal(&state, &auth, id, item).await?))
}
