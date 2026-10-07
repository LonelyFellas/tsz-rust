use super::{dto::*, service};
use crate::{
    api::{ApiJson, ApiPath, ApiQuery},
    auth::extract::AuthUser,
    error::AppError,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use uuid::Uuid;
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/me/learning-tasks/preview", post(preview))
        .route("/api/v1/me/learning-tasks", get(list).post(create))
        .route("/api/v1/me/learning-tasks/{id}", get(detail).patch(rename))
        .route("/api/v1/me/learning-tasks/{id}/archive", post(archive))
        .route(
            "/api/v1/me/learning-tasks/{id}/runs",
            get(history).post(start),
        )
        .route("/api/v1/me/learning-runs/{id}", get(get_run))
        .route(
            "/api/v1/me/learning-runs/{id}/questions",
            get(get_questions),
        )
        .route("/api/v1/me/learning-runs/{id}/answers", post(answer))
}
#[utoipa::path(post,path="/api/v1/me/learning-tasks/preview",tag="learning_tasks",security(("bearer_auth"=[])),request_body=PreviewLearningTask,responses((status=200,description="本人学习事实",body=LearningPreview),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要已绑定手机的学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、请求键、轮次或来源冲突"),(status=422,description="JSON不符合契约")))]
pub async fn preview(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(input): ApiJson<PreviewLearningTask>,
) -> Result<Json<LearningPreview>, AppError> {
    Ok(Json(service::preview(&state.pool, &auth, input).await?))
}
#[utoipa::path(post,path="/api/v1/me/learning-tasks",tag="learning_tasks",security(("bearer_auth"=[])),request_body=CreateLearningTask,responses((status=200,description="本人学习事实",body=LearningTask),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要已绑定手机的学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、请求键、轮次或来源冲突"),(status=422,description="JSON不符合契约")))]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(input): ApiJson<CreateLearningTask>,
) -> Result<Json<LearningTask>, AppError> {
    Ok(Json(service::create(&state.pool, &auth, input).await?))
}
#[utoipa::path(get,path="/api/v1/me/learning-tasks",tag="learning_tasks",security(("bearer_auth"=[])),params(LearningPageQuery),responses((status=200,description="本人学习事实",body=LearningTaskPage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要已绑定手机的学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、请求键、轮次或来源冲突"),(status=422,description="JSON不符合契约")))]
pub async fn list(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(query): ApiQuery<LearningPageQuery>,
) -> Result<Json<LearningTaskPage>, AppError> {
    Ok(Json(service::list(&state.pool, &auth, query).await?))
}
#[utoipa::path(get,path="/api/v1/me/learning-tasks/{id}",tag="learning_tasks",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,description="本人学习事实",body=LearningTaskDetail),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要已绑定手机的学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、请求键、轮次或来源冲突"),(status=422,description="JSON不符合契约")))]
pub async fn detail(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<LearningTaskDetail>, AppError> {
    Ok(Json(service::detail(&state.pool, &auth, id).await?))
}
#[utoipa::path(patch,path="/api/v1/me/learning-tasks/{id}",tag="learning_tasks",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=RenameLearningTask,responses((status=200,description="本人学习事实",body=LearningTask),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要已绑定手机的学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、请求键、轮次或来源冲突"),(status=422,description="JSON不符合契约")))]
pub async fn rename(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<RenameLearningTask>,
) -> Result<Json<LearningTask>, AppError> {
    Ok(Json(service::rename(&state.pool, &auth, id, input).await?))
}
#[utoipa::path(post,path="/api/v1/me/learning-tasks/{id}/archive",tag="learning_tasks",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=ArchiveLearningTask,responses((status=200,description="本人学习事实",body=LearningTask),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要已绑定手机的学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、请求键、轮次或来源冲突"),(status=422,description="JSON不符合契约")))]
pub async fn archive(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<ArchiveLearningTask>,
) -> Result<Json<LearningTask>, AppError> {
    Ok(Json(service::archive(&state.pool, &auth, id, input).await?))
}
#[utoipa::path(post,path="/api/v1/me/learning-tasks/{id}/runs",tag="learning_tasks",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=StartLearningRun,responses((status=200,description="本人学习事实",body=LearningRun),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要已绑定手机的学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、请求键、轮次或来源冲突"),(status=422,description="JSON不符合契约")))]
pub async fn start(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<StartLearningRun>,
) -> Result<Json<LearningRun>, AppError> {
    Ok(Json(service::start(&state.pool, &auth, id, input).await?))
}
#[utoipa::path(get,path="/api/v1/me/learning-tasks/{id}/runs",tag="learning_tasks",security(("bearer_auth"=[])),params(("id"=Uuid,Path),LearningPageQuery),responses((status=200,description="本人学习事实",body=LearningRunPage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要已绑定手机的学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、请求键、轮次或来源冲突"),(status=422,description="JSON不符合契约")))]
pub async fn history(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<LearningPageQuery>,
) -> Result<Json<LearningRunPage>, AppError> {
    Ok(Json(service::history(&state.pool, &auth, id, query).await?))
}
#[utoipa::path(get,path="/api/v1/me/learning-runs/{id}",tag="learning_tasks",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,description="本人学习事实",body=LearningRun),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要已绑定手机的学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、请求键、轮次或来源冲突"),(status=422,description="JSON不符合契约")))]
pub async fn get_run(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<LearningRun>, AppError> {
    Ok(Json(service::get_run(&state.pool, &auth, id).await?))
}
#[utoipa::path(get,path="/api/v1/me/learning-runs/{id}/questions",tag="learning_tasks",security(("bearer_auth"=[])),params(("id"=Uuid,Path),LearningPageQuery),responses((status=200,description="本人学习事实",body=LearningQuestionPage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要已绑定手机的学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、请求键、轮次或来源冲突"),(status=422,description="JSON不符合契约")))]
pub async fn get_questions(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<LearningPageQuery>,
) -> Result<Json<LearningQuestionPage>, AppError> {
    Ok(Json(
        service::get_questions(&state.pool, &auth, id, query).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/me/learning-runs/{id}/answers",tag="learning_tasks",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=SubmitLearningAnswer,responses((status=200,description="本人学习事实",body=LearningAnswerReceipt),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="需要已绑定手机的学生资格"),(status=404,description="资源不存在"),(status=409,description="版本、请求键、轮次或来源冲突"),(status=422,description="JSON不符合契约")))]
pub async fn answer(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<SubmitLearningAnswer>,
) -> Result<Json<LearningAnswerReceipt>, AppError> {
    Ok(Json(service::answer(&state.pool, &auth, id, input).await?))
}
