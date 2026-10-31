use super::{dto::*, *};
use crate::{
    api::{ApiJson, ApiPath, ApiQuery},
    state::AppState,
};
use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    routing::{get, post},
};
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/me/learning-plans/preview", post(preview))
        .route("/api/v1/me/learning-plans", get(list).post(create))
        .route("/api/v1/me/learning-plans/{id}", get(detail).patch(rename))
        .route("/api/v1/me/learning-plans/{id}/archive", post(archive))
        .route(
            "/api/v1/me/learning-plans/{id}/runs",
            get(history).post(start),
        )
        .route(
            "/api/v1/me/learning-plans/{id}/cycles/preview",
            post(cycle_preview),
        )
        .route(
            "/api/v1/me/learning-plans/{id}/cycles",
            get(cycles).post(cycle_create),
        )
}
#[utoipa::path(post,path="/api/v1/me/learning-plans/preview",tag="learning_plans",security(("bearer_auth"=[])),request_body=PreviewLearningPlan,responses((status=200,description="学习计划",body=LearningPlanPreview),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、来源、预览或请求键冲突"),(status=422,description="JSON不符合契约")))]
pub async fn preview(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    ApiJson(input): ApiJson<PreviewLearningPlan>,
) -> Result<Json<LearningPlanPreview>, AppError> {
    crate::question_bank::learning::require_strict(&headers, input.choice_policy.as_deref())?;
    crate::question_bank::learning::require_capability(&headers, input.question_type.as_deref())?;
    crate::question_bank::learning::require_wordlist_capability(
        &headers,
        input.question_type.as_deref(),
    )?;
    super::super::mastery::require(&headers, input.practice_mode.as_deref())?;
    Ok(Json(super::preview(&state.pool, &auth, input).await?))
}
#[utoipa::path(post,path="/api/v1/me/learning-plans",tag="learning_plans",security(("bearer_auth"=[])),request_body=CreateLearningPlan,responses((status=200,description="学习计划",body=LearningPlan),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、来源、预览或请求键冲突"),(status=422,description="JSON不符合契约")))]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    ApiJson(input): ApiJson<CreateLearningPlan>,
) -> Result<Json<LearningPlan>, AppError> {
    crate::question_bank::learning::require_strict(&headers, input.choice_policy.as_deref())?;
    crate::question_bank::learning::require_capability(&headers, input.question_type.as_deref())?;
    crate::question_bank::learning::require_wordlist_capability(
        &headers,
        input.question_type.as_deref(),
    )?;
    super::super::mastery::require(&headers, input.practice_mode.as_deref())?;
    Ok(Json(super::create(&state.pool, &auth, input).await?))
}
#[utoipa::path(get,path="/api/v1/me/learning-plans",tag="learning_plans",security(("bearer_auth"=[])),params(LearningPageQuery),responses((status=200,description="学习计划",body=LearningPlanPage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、来源、预览或请求键冲突"),(status=422,description="JSON不符合契约")))]
pub async fn list(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<LearningPageQuery>,
) -> Result<Json<LearningPlanPage>, AppError> {
    Ok(Json(
        super::list(
            &state.pool,
            &auth,
            query,
            crate::question_bank::learning::capable(&headers),
            super::super::mastery::capable(&headers),
        )
        .await?,
    ))
}
#[utoipa::path(get,path="/api/v1/me/learning-plans/{id}",tag="learning_plans",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,description="学习计划",body=LearningPlan),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、来源、预览或请求键冲突"),(status=422,description="JSON不符合契约")))]
pub async fn detail(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<LearningPlan>, AppError> {
    Ok(Json(super::detail(&state.pool, &auth, id).await?))
}
#[utoipa::path(patch,path="/api/v1/me/learning-plans/{id}",tag="learning_plans",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=RenameLearningTask,responses((status=200,description="学习计划",body=LearningPlan),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、来源、预览或请求键冲突"),(status=422,description="JSON不符合契约")))]
pub async fn rename(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<RenameLearningTask>,
) -> Result<Json<LearningPlan>, AppError> {
    Ok(Json(super::rename(&state.pool, &auth, id, input).await?))
}
#[utoipa::path(post,path="/api/v1/me/learning-plans/{id}/archive",tag="learning_plans",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=ArchiveLearningTask,responses((status=200,description="学习计划",body=LearningPlan),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、来源、预览或请求键冲突"),(status=422,description="JSON不符合契约")))]
pub async fn archive(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<ArchiveLearningTask>,
) -> Result<Json<LearningPlan>, AppError> {
    Ok(Json(super::archive(&state.pool, &auth, id, input).await?))
}
#[utoipa::path(post,path="/api/v1/me/learning-plans/{id}/runs",tag="learning_plans",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=StartLearningRun,responses((status=200,description="学习计划",body=LearningRun),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、来源、预览或请求键冲突"),(status=422,description="JSON不符合契约")))]
pub async fn start(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<StartLearningRun>,
) -> Result<Json<LearningRun>, AppError> {
    Ok(Json(super::start(&state.pool, &auth, id, input).await?))
}
#[utoipa::path(get,path="/api/v1/me/learning-plans/{id}/runs",tag="learning_plans",security(("bearer_auth"=[])),params(("id"=Uuid,Path),LearningPageQuery),responses((status=200,description="学习计划",body=LearningPlanRunPage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、来源、预览或请求键冲突"),(status=422,description="JSON不符合契约")))]
pub async fn history(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<LearningPageQuery>,
) -> Result<Json<LearningPlanRunPage>, AppError> {
    Ok(Json(super::history(&state.pool, &auth, id, query).await?))
}
#[utoipa::path(post,path="/api/v1/me/learning-plans/{id}/cycles/preview",tag="learning_plans",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=PreviewLearningCycle,responses((status=200,description="学习计划",body=LearningCyclePreview),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、来源、预览或请求键冲突"),(status=422,description="JSON不符合契约")))]
pub async fn cycle_preview(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<PreviewLearningCycle>,
) -> Result<Json<LearningCyclePreview>, AppError> {
    Ok(Json(
        super::cycle_preview(&state.pool, &auth, id, input).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/me/learning-plans/{id}/cycles",tag="learning_plans",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=CreateLearningCycle,responses((status=200,description="学习计划",body=LearningCycle),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、来源、预览或请求键冲突"),(status=422,description="JSON不符合契约")))]
pub async fn cycle_create(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<CreateLearningCycle>,
) -> Result<Json<LearningCycle>, AppError> {
    Ok(Json(
        super::cycle_create(&state.pool, &auth, id, input).await?,
    ))
}
#[utoipa::path(get,path="/api/v1/me/learning-plans/{id}/cycles",tag="learning_plans",security(("bearer_auth"=[])),params(("id"=Uuid,Path),LearningPageQuery),responses((status=200,description="学习计划",body=LearningCyclePage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、来源、预览或请求键冲突"),(status=422,description="JSON不符合契约")))]
pub async fn cycles(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<LearningPageQuery>,
) -> Result<Json<LearningCyclePage>, AppError> {
    Ok(Json(super::cycles(&state.pool, &auth, id, query).await?))
}
