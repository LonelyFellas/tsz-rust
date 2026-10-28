use super::{batch, dto::*, service, source};
use crate::{
    admin::AdminAuth,
    api::{ApiJson, ApiPath, ApiQuery},
    error::AppError,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    routing::{get, post},
};
use uuid::Uuid;
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/question-bank/candidate-page",
            get(candidate_page),
        )
        .route(
            "/api/v1/admin/question-bank/batches/preview",
            post(batch_preview),
        )
        .route(
            "/api/v1/admin/question-bank/batches",
            get(batches).post(create_batch),
        )
        .route(
            "/api/v1/admin/question-bank/batches/{id}",
            get(batch_detail),
        )
        .route(
            "/api/v1/admin/question-bank/batches/{id}/items",
            get(batch_items),
        )
        .route(
            "/api/v1/admin/question-bank/batches/{id}/cancel",
            post(cancel_batch),
        )
        .route(
            "/api/v1/admin/question-bank/candidates",
            get(candidate_list),
        )
        .route(
            "/api/v1/admin/question-bank/generations",
            get(generations).post(create_generation),
        )
        .route(
            "/api/v1/admin/question-bank/generations/{id}",
            get(generation),
        )
        .route(
            "/api/v1/admin/question-bank/generations/{id}/cancel",
            post(cancel_generation),
        )
        .route("/api/v1/admin/question-bank/questions", get(questions))
        .route(
            "/api/v1/admin/question-bank/questions/{id}",
            get(question).patch(edit_question),
        )
        .route(
            "/api/v1/admin/question-bank/questions/{id}/review",
            post(review_question),
        )
        .route("/api/internal/agent/v1/requests/{id}/context", get(context))
        .route(
            "/api/internal/agent/v1/requests/{id}/candidates/search",
            post(search),
        )
        .route(
            "/api/internal/agent/v1/requests/{id}/entries/batch-get",
            post(batch_get),
        )
        .route("/api/v1/me/learning-capabilities", get(capabilities))
}
#[utoipa::path(get,path="/api/v1/admin/question-bank/candidates",tag="question_bank",security(("bearer_auth"=[])),params(QuestionCandidateQuery),responses((status=200,description="题库操作结果",body=QuestionCandidatePage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="版本或来源冲突"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn candidate_list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(input): ApiQuery<QuestionCandidateQuery>,
) -> Result<Json<QuestionCandidatePage>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    service::authorize(&mut tx, &auth, "words.access", None).await?;
    let result = service::candidates(&mut tx, &input).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(result))
}
#[utoipa::path(get,path="/api/v1/admin/question-bank/generations",tag="question_bank",security(("bearer_auth"=[])),params(QuestionBankQuery),responses((status=200,description="题库操作结果",body=QuestionGenerationPage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="版本或来源冲突"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn generations(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(input): ApiQuery<QuestionBankQuery>,
) -> Result<Json<QuestionGenerationPage>, AppError> {
    Ok(Json(
        service::list_generations(&state.pool, &auth, input).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/admin/question-bank/generations",tag="question_bank",security(("bearer_auth"=[])),request_body=CreateQuestionGeneration,responses((status=200,description="题库操作结果",body=QuestionGeneration),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="版本或来源冲突"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn create_generation(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<CreateQuestionGeneration>,
) -> Result<Json<QuestionGeneration>, AppError> {
    if state.vocabulary_agent.is_none() {
        return Err(AppError::unavailable(
            crate::error::ErrorCode::ServiceUnavailable,
            "生成服务尚未配置",
        ));
    }
    Ok(Json(service::create(&state.pool, &auth, input).await?))
}
#[utoipa::path(get,path="/api/v1/admin/question-bank/generations/{id}",tag="question_bank",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,description="题库操作结果",body=QuestionGeneration),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="版本或来源冲突"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn generation(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<QuestionGeneration>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    service::authorize(&mut tx, &auth, "words.access", None).await?;
    let result = service::generation(&mut tx, id).await?.view();
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(result))
}
#[utoipa::path(post,path="/api/v1/admin/question-bank/generations/{id}/cancel",tag="question_bank",security(("bearer_auth"=[])),request_body=CancelQuestionGeneration,params(("id"=Uuid,Path)),responses((status=200,description="题库操作结果",body=QuestionGeneration),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="版本或来源冲突"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn cancel_generation(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<CancelQuestionGeneration>,
) -> Result<Json<QuestionGeneration>, AppError> {
    Ok(Json(service::cancel(&state.pool, &auth, id, input).await?))
}
#[utoipa::path(get,path="/api/v1/admin/question-bank/questions",tag="question_bank",security(("bearer_auth"=[])),params(QuestionBankQuery),responses((status=200,description="题库操作结果",body=BankQuestionPage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="版本或来源冲突"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn questions(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(input): ApiQuery<QuestionBankQuery>,
) -> Result<Json<BankQuestionPage>, AppError> {
    Ok(Json(
        service::list_questions(&state.pool, &auth, input).await?,
    ))
}
#[utoipa::path(get,path="/api/v1/admin/question-bank/questions/{id}",tag="question_bank",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,description="题库操作结果",body=BankQuestion),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="版本或来源冲突"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn question(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<BankQuestion>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    service::authorize(&mut tx, &auth, "words.access", None).await?;
    let result = service::question(&mut tx, id).await?.view();
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(result))
}
#[utoipa::path(patch,path="/api/v1/admin/question-bank/questions/{id}",tag="question_bank",security(("bearer_auth"=[])),request_body=EditBankQuestion,params(("id"=Uuid,Path)),responses((status=200,description="题库操作结果",body=BankQuestion),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="版本或来源冲突"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn edit_question(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<EditBankQuestion>,
) -> Result<Json<BankQuestion>, AppError> {
    Ok(Json(service::edit(&state.pool, &auth, id, input).await?))
}
#[utoipa::path(post,path="/api/v1/admin/question-bank/questions/{id}/review",tag="question_bank",security(("bearer_auth"=[])),request_body=ReviewBankQuestion,params(("id"=Uuid,Path)),responses((status=200,description="题库操作结果",body=BankQuestion),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="版本或来源冲突"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn review_question(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<ReviewBankQuestion>,
) -> Result<Json<BankQuestion>, AppError> {
    Ok(Json(service::review(&state.pool, &auth, id, input).await?))
}
fn internal_auth(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let config = state
        .vocabulary_agent
        .as_ref()
        .ok_or_else(|| AppError::forbidden(crate::error::ErrorCode::Forbidden, "服务身份无效"))?;
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    use sha2::{Digest, Sha256};
    let supplied = Sha256::digest(token.as_bytes());
    let expected = Sha256::digest(config.inbound_token.as_bytes());
    if supplied
        .iter()
        .zip(expected.iter())
        .fold(0u8, |v, (a, b)| v | (a ^ b))
        != 0
    {
        return Err(AppError::unauthorized(
            crate::error::ErrorCode::InvalidToken,
            "服务身份无效",
        ));
    }
    Ok(())
}
#[utoipa::path(get,path="/api/internal/agent/v1/requests/{id}/context",tag="question_bank",params(("id"=Uuid,Path)),responses((status=200,description="委托上下文",body=AgentGenerationContext),(status=401,description="服务身份无效"),(status=403,description="委托失效"),(status=409,description="来源变化")))]
pub async fn context(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<AgentGenerationContext>, AppError> {
    internal_auth(&state, &headers)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let row = service::delegation(&mut tx, id).await?;
    let context = AgentGenerationContext {
        contract_version: 1,
        external_request_id: id,
        target: row.target.0,
        question_level: row.question_level,
        answer_profile: row.answer_profile,
    };
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(context))
}
#[utoipa::path(post,path="/api/internal/agent/v1/requests/{id}/candidates/search",tag="question_bank",params(("id"=Uuid,Path)),request_body=AgentCandidateSearch,responses((status=200,description="候选词义",body=QuestionCandidatePage),(status=400,description="无效请求"),(status=401,description="服务身份无效"),(status=403,description="委托失效"),(status=409,description="来源变化")))]
pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<AgentCandidateSearch>,
) -> Result<Json<QuestionCandidatePage>, AppError> {
    internal_auth(&state, &headers)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let row = service::delegation(&mut tx, id).await?;
    if input.contract_version != 1
        || input.limit != 40
        || input.exclude_candidate_refs.len() > 80
        || input.target_source != row.target.source
        || input.question_level != row.question_level
        || input.answer_profile != row.answer_profile
    {
        return Err(super::invalid("检索不属于此委托"));
    }
    let page = service::agent_candidates(&mut tx, &row.target, &input).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(page))
}
#[utoipa::path(post,path="/api/internal/agent/v1/requests/{id}/entries/batch-get",tag="question_bank",params(("id"=Uuid,Path)),request_body=AgentEntriesRequest,responses((status=200,description="独立来源复核",body=QuestionCandidatePage),(status=400,description="无效请求"),(status=401,description="服务身份无效"),(status=403,description="委托失效"),(status=409,description="来源变化")))]
pub async fn batch_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<AgentEntriesRequest>,
) -> Result<Json<QuestionCandidatePage>, AppError> {
    internal_auth(&state, &headers)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let row = service::delegation(&mut tx, id).await?;
    if input.contract_version != 1
        || input.items.is_empty()
        || input.items.len() > 80
        || input
            .items
            .iter()
            .any(|i| i.answer_profile != row.answer_profile)
    {
        return Err(super::invalid("复核范围无效"));
    }
    let mut items = vec![];
    for item in &input.items {
        items.push(source::read(&mut tx, item).await?);
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(QuestionCandidatePage {
        contract_version: 1,
        items,
    }))
}
#[utoipa::path(get,path="/api/v1/me/learning-capabilities",tag="learning_plans",security(("bearer_auth"=[])),responses((status=200,description="支持的学习题型",body=LearningCapabilities),(status=401,description="需要登录")))]
pub async fn capabilities(_auth: crate::auth::extract::AuthUser) -> Json<LearningCapabilities> {
    Json(LearningCapabilities {
        question_types: vec!["spelling_zh_to_en".into(), "choice_zh_to_en".into()],
    })
}

#[utoipa::path(get,path="/api/v1/admin/question-bank/candidate-page",tag="question_bank",security(("bearer_auth"=[])),params(QuestionCandidatePageQuery),responses((status=200,description="批量命题操作结果",body=QuestionCandidateSearchPage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="覆盖情况已变化"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn candidate_page(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(input): ApiQuery<QuestionCandidatePageQuery>,
) -> Result<Json<QuestionCandidateSearchPage>, AppError> {
    Ok(Json(
        batch::candidate_page(&state.pool, &auth, input).await?,
    ))
}

#[utoipa::path(post,path="/api/v1/admin/question-bank/batches/preview",tag="question_bank",security(("bearer_auth"=[])),request_body=QuestionBatchSelection,responses((status=200,description="批量命题操作结果",body=QuestionBatchPreview),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="覆盖情况已变化"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn batch_preview(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<QuestionBatchSelection>,
) -> Result<Json<QuestionBatchPreview>, AppError> {
    Ok(Json(batch::preview(&state.pool, &auth, input).await?))
}

#[utoipa::path(get,path="/api/v1/admin/question-bank/batches",tag="question_bank",security(("bearer_auth"=[])),params(QuestionBankQuery),responses((status=200,description="批量命题操作结果",body=QuestionBatchPage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="覆盖情况已变化"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn batches(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(input): ApiQuery<QuestionBankQuery>,
) -> Result<Json<QuestionBatchPage>, AppError> {
    Ok(Json(batch::list(&state.pool, &auth, input).await?))
}

#[utoipa::path(post,path="/api/v1/admin/question-bank/batches",tag="question_bank",security(("bearer_auth"=[])),request_body=CreateQuestionBatch,responses((status=200,description="批量命题操作结果",body=QuestionBatch),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="覆盖情况已变化"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn create_batch(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<CreateQuestionBatch>,
) -> Result<Json<QuestionBatch>, AppError> {
    if state.vocabulary_agent.is_none() {
        return Err(AppError::unavailable(
            crate::error::ErrorCode::ServiceUnavailable,
            "生成服务尚未配置",
        ));
    }
    Ok(Json(batch::create(&state.pool, &auth, input).await?))
}

#[utoipa::path(get,path="/api/v1/admin/question-bank/batches/{id}",tag="question_bank",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,description="批量命题操作结果",body=QuestionBatch),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="覆盖情况已变化"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn batch_detail(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<QuestionBatch>, AppError> {
    Ok(Json(batch::get(&state.pool, &auth, id).await?))
}

#[utoipa::path(get,path="/api/v1/admin/question-bank/batches/{id}/items",tag="question_bank",security(("bearer_auth"=[])),params(("id"=Uuid,Path),QuestionBankQuery),responses((status=200,description="批量命题操作结果",body=QuestionGenerationPage),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="覆盖情况已变化"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn batch_items(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(input): ApiQuery<QuestionBankQuery>,
) -> Result<Json<QuestionGenerationPage>, AppError> {
    Ok(Json(batch::items(&state.pool, &auth, id, input).await?))
}

#[utoipa::path(post,path="/api/v1/admin/question-bank/batches/{id}/cancel",tag="question_bank",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,description="批量命题操作结果",body=QuestionBatch),(status=400,description="参数无效"),(status=401,description="会话失效"),(status=403,description="无操作权限"),(status=404,description="不存在"),(status=409,description="覆盖情况已变化"),(status=422,description="格式无效"),(status=503,description="生成服务不可用")))]
pub async fn cancel_batch(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<QuestionBatch>, AppError> {
    Ok(Json(batch::cancel(&state.pool, &auth, id).await?))
}
