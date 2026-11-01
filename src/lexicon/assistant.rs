//! Read-only assistant: the model proposes tools; the authenticated API executes them.
use axum::{
    Json,
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};
use deadpool_redis::redis::Script;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    admin::{AdminAuth, permissions},
    api::ApiJson,
    error::{AppError, ErrorCode},
    lexicon::{
        dto::{AdminWordListQuery, WordCreationStep},
        repository::LexiconRepository,
    },
    state::AppState,
};

const MAX_CALLS: u32 = 4;
const MAX_TOOLS: usize = 6;
#[path = "assistant_candidates.rs"]
pub mod candidates;
const RULES: &str = "填写规则 v1：基础信息用于确认词条与词性；词形与发音按当前词性配置填写，英美无差异时共用，有证据才区分。不同拼写应有各自的词形内容，不能把原形内容直接共用给不同拼写的复数等变体。实际发音采用平台天生之®通用英语音标字母表，不等同于 IPA；缺少规范依据不能编造转换或用 IPA 直接替代。词义与例句按具体词性、词义组织，中文/英文释义应指向同一义项，例句应体现目标义项，CEFR 是教学等级建议，不是词典事实。语法结构说明具体用法，细分词性以当前目录为准。当前编辑可能未保存；完成情况不能代替服务端发布校验，也不能据此把后续步骤尚未填写的内容判成当前步骤错误。词频没有可信来源时不能猜测。AI 只提供建议，不保存或发布。";

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconAssistantHistory {
    pub role: AssistantHistoryRole,
    #[schema(min_length = 1, max_length = 4000)]
    pub content: String,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistantHistoryRole {
    User,
    Assistant,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconAssistantInput {
    pub entry_id: Uuid,
    #[schema(minimum = 1)]
    pub base_revision: i64,
    pub context_id: Uuid,
    pub step: WordCreationStep,
    #[schema(max_length = 24000)]
    pub local_content: String,
    pub context_truncated: bool,
    #[schema(min_length = 1, max_length = 2000)]
    pub question: String,
    #[schema(max_items = 6)]
    pub history: Vec<LexiconAssistantHistory>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssistantSourceKind {
    SmartLexicon,
    BuiltinDictionary,
    FillingRules,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconAssistantSource {
    pub kind: AssistantSourceKind,
    pub label: String,
    #[schema(required = true)]
    pub entry_id: Option<Uuid>,
    #[schema(required = true)]
    pub version: Option<String>,
    pub truncated: bool,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconAssistantUsage {
    pub model: String,
    pub model_calls: u32,
    pub tool_calls: u32,
    #[schema(required = true)]
    pub input_tokens: Option<u64>,
    #[schema(required = true)]
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconAssistantResponse {
    pub entry_id: Uuid,
    pub context_id: Uuid,
    pub answer: String,
    pub sources: Vec<LexiconAssistantSource>,
    pub usage: LexiconAssistantUsage,
    pub requires_human_review: bool,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconAssistantCapability {
    pub available: bool,
}

#[derive(Serialize, ToSchema)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum LexiconAssistantEvent {
    Status {
        message: String,
    },
    Delta {
        text: String,
    },
    Reset,
    Complete {
        response: Box<LexiconAssistantResponse>,
    },
    Failed {
        status: u16,
    },
}

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum ModelEvent {
    Delta { text: String },
    Complete { step: ModelStep },
    Failed,
}

async fn emit(
    sender: Option<&tokio::sync::mpsc::Sender<LexiconAssistantEvent>>,
    event: LexiconAssistantEvent,
) -> Result<(), AppError> {
    if let Some(sender) = sender {
        sender.send(event).await.map_err(|_| unavailable())?;
    }
    Ok(())
}

#[utoipa::path(post,path="/api/v1/admin/lexicon/assistant/stream",tag="admin-lexicon",security(("bearer_auth"=[])),request_body=LexiconAssistantInput,
    responses((status=200,body=LexiconAssistantEvent,content_type="application/x-ndjson",description="逐行 JSON：查询状态、实际文本增量、最终来源或失败"),
    (status=401,description="身份失效"),(status=403,description="缺少 words.access"),(status=404,description="词条不存在"),(status=409,description="版本变化"),(status=422,description="输入无效"),(status=429,description="提问过于频繁"),(status=503,description="服务不可用")))]
pub async fn ask_stream(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<LexiconAssistantInput>,
) -> Result<Response, AppError> {
    validate(&input)?;
    authorize(&state, &auth).await?;
    if state.vocabulary_agent.is_none() {
        return Err(unavailable());
    }
    check_revision(&state, &input).await?;
    let lock = format!(
        "{}lexicon-assistant:pending:{}",
        state.login_throttle_prefix, auth.subject
    );
    let receipt = Uuid::now_v7().to_string();
    reserve(&state, &lock, &receipt).await?;
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move {
        let work = async {
            emit(
                Some(&sender),
                LexiconAssistantEvent::Status {
                    message: "正在读取当前编辑内容…".into(),
                },
            )
            .await?;
            let output = run(&state, &auth, &input, Some(&sender)).await?;
            authorize(&state, &auth).await?;
            check_revision(&state, &input).await?;
            emit(
                Some(&sender),
                LexiconAssistantEvent::Complete {
                    response: Box::new(output),
                },
            )
            .await
        };
        let result = tokio::select! {
            _=sender.closed()=>Ok(()),
            result=tokio::time::timeout(Duration::from_secs(75),work)=>result.map_err(|_|unavailable()).and_then(|v|v),
        };
        if let Err(error) = result {
            let status = error.into_response().status().as_u16();
            let _ = sender.send(LexiconAssistantEvent::Failed { status }).await;
        }
        if let Ok(mut conn) = state.redis.get().await {
            let _=Script::new("if redis.call('GET',KEYS[1])==ARGV[1] then return redis.call('DEL',KEYS[1]) end return 0")
                .key(lock).arg(receipt).invoke_async::<i64>(&mut conn).await;
        }
    });
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        let event = receiver.recv().await?;
        let bytes = axum::body::Bytes::from(
            serde_json::to_string(&event).expect("assistant event serializes") + "\n",
        );
        Some((Ok::<_, std::convert::Infallible>(bytes), receiver))
    });
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CACHE_CONTROL, "no-store"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}

async fn agent_stream_request(
    state: &AppState,
    auth: &AdminAuth,
    input: &LexiconAssistantInput,
    body: Value,
    sender: &tokio::sync::mpsc::Sender<LexiconAssistantEvent>,
) -> Result<ModelStep, AppError> {
    let config = state.vocabulary_agent.as_ref().ok_or_else(unavailable)?;
    let mut response = config
        .client
        .post(format!(
            "{}/v1/lexicon-assistant/step/stream",
            config.base_url
        ))
        .bearer_auth(&config.outbound_token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .timeout(Duration::from_secs(36))
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(unavailable());
    }
    let mut buffer = Vec::new();
    let mut total = 0;
    let mut complete = None;
    let mut text = String::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        total += chunk.len();
        if total > 100000 {
            return Err(unavailable());
        }
        buffer.extend(chunk);
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            if complete.is_some() {
                return Err(unavailable());
            }
            match serde_json::from_slice::<ModelEvent>(&line).map_err(|_| unavailable())? {
                ModelEvent::Delta { text: delta } => {
                    authorize(state, auth).await?;
                    check_revision(state, input).await?;
                    text.push_str(&delta);
                    if text.chars().count() > 16000 {
                        return Err(unavailable());
                    }
                    emit(Some(sender), LexiconAssistantEvent::Delta { text: delta }).await?;
                }
                ModelEvent::Complete { step } => {
                    if step.content != text {
                        return Err(unavailable());
                    }
                    complete = Some(step);
                }
                ModelEvent::Failed => return Err(unavailable()),
            }
        }
    }
    if !buffer.is_empty() {
        return Err(unavailable());
    }
    complete.ok_or_else(unavailable)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ToolCall {
    id: String,
    name: ToolName,
    arguments: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ToolName {
    SearchLexicon,
    ReadLexicon,
    LookupDictionary,
    ReadFillingRules,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelStep {
    content: String,
    tool_calls: Vec<ToolCall>,
    model: String,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    q: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryArgs {
    entry_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

fn invalid(message: &'static str) -> AppError {
    AppError::unprocessable(ErrorCode::InvalidRequestBody, message)
}
fn unavailable() -> AppError {
    AppError::unavailable(
        ErrorCode::ServiceUnavailable,
        "词库助手暂时不可用，请稍后重试；当前编辑已保留",
    )
}

fn validate(input: &LexiconAssistantInput) -> Result<(), AppError> {
    if input.base_revision < 1
        || input.question.trim().is_empty()
        || input.question.chars().count() > 2000
        || input.local_content.chars().count() > 24000
        || input.history.len() > 6
        || input
            .history
            .iter()
            .any(|m| m.content.is_empty() || m.content.chars().count() > 4000)
    {
        return Err(invalid("问题或上下文过长，请缩小问题范围后重试"));
    }
    Ok(())
}

#[utoipa::path(get,path="/api/v1/admin/lexicon/assistant/capability",tag="admin-lexicon",security(("bearer_auth"=[])),
    responses((status=200,body=LexiconAssistantCapability,description="词库助手配置状态"),(status=401,description="身份失效"),(status=403,description="缺少 words.access")))]
pub async fn capability(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Response, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    let available = if state.vocabulary_agent.is_some() {
        agent_request(&state, "capability", None)
            .await
            .ok()
            .and_then(|v| serde_json::from_value::<LexiconAssistantCapability>(v).ok())
            .is_some_and(|v| v.available)
    } else {
        false
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(LexiconAssistantCapability { available }),
    )
        .into_response())
}

#[utoipa::path(post,path="/api/v1/admin/lexicon/assistant",tag="admin-lexicon",security(("bearer_auth"=[])),request_body=LexiconAssistantInput,
    responses((status=200,body=LexiconAssistantResponse,description="只读建议和实际查询来源"),(status=401,description="身份失效"),(status=403,description="缺少 words.access"),
    (status=404,description="词条不存在"),(status=409,description="词条版本变化"),(status=422,description="输入无效"),(status=429,description="问答过于频繁或仍在进行"),(status=503,description="助手不可用、超时或输出无效")))]
pub async fn ask(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<LexiconAssistantInput>,
) -> Result<Response, AppError> {
    validate(&input)?;
    authorize(&state, &auth).await?;
    if state.vocabulary_agent.is_none() {
        return Err(unavailable());
    }
    check_revision(&state, &input).await?;
    let lock = format!(
        "{}lexicon-assistant:pending:{}",
        state.login_throttle_prefix, auth.subject
    );
    let receipt = Uuid::now_v7().to_string();
    reserve(&state, &lock, &receipt).await?;
    let result = tokio::time::timeout(Duration::from_secs(75), run(&state, &auth, &input, None))
        .await
        .map_err(|_| unavailable())
        .and_then(|v| v);
    // Generation identity prevents a timed-out request from releasing a newer reservation.
    if let Ok(mut conn) = state.redis.get().await {
        let _ = Script::new("if redis.call('GET',KEYS[1])==ARGV[1] then return redis.call('DEL',KEYS[1]) end return 0")
            .key(&lock).arg(&receipt).invoke_async::<i64>(&mut conn).await;
    }
    let output = result?;
    authorize(&state, &auth).await?;
    check_revision(&state, &input).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(output)).into_response())
}

async fn authorize(state: &AppState, auth: &AdminAuth) -> Result<(), AppError> {
    permissions::reload(state, auth)
        .await?
        .require("words.access")?;
    if !state.smart_lexicon_v3_flags.read {
        return Err(unavailable());
    }
    Ok(())
}

async fn check_revision(state: &AppState, input: &LexiconAssistantInput) -> Result<(), AppError> {
    let revision: Option<i64> = sqlx::query_scalar(
        "SELECT revision FROM lexicon.entries WHERE id=$1 AND content_schema_version=3",
    )
    .bind(input.entry_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::internal)?;
    let revision = revision.ok_or_else(|| {
        super::handler::map_error(super::service::LexiconServiceError::WordNotFound)
    })?;
    if revision != input.base_revision {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "词条版本已更新，请核对当前编辑后重新提问",
        ));
    }
    Ok(())
}

async fn reserve(state: &AppState, key: &str, receipt: &str) -> Result<(), AppError> {
    let mut conn = state.redis.get().await.map_err(|_| unavailable())?;
    let accepted: bool = Script::new(
        r#"
        if redis.call('EXISTS',KEYS[1])==1 then return 0 end
        if tonumber(redis.call('GET',KEYS[2]) or '0')>=10 then return 0 end
        redis.call('SET',KEYS[1],ARGV[1],'EX',90)
        local n=redis.call('INCR',KEYS[2])
        if n==1 then redis.call('EXPIRE',KEYS[2],60) end
        return 1
    "#,
    )
    .key(key)
    .key(format!("{key}:rate"))
    .arg(receipt)
    .invoke_async(&mut conn)
    .await
    .map_err(|_| unavailable())?;
    if !accepted {
        return Err(AppError::rate_limited(
            ErrorCode::LexiconAssistantRateLimited,
            "助手正在回答或提问过于频繁，请稍后重试",
        )
        .with_retry_after(10));
    }
    Ok(())
}

async fn agent_request(
    state: &AppState,
    operation: &str,
    body: Option<Value>,
) -> Result<Value, AppError> {
    let config = state.vocabulary_agent.as_ref().ok_or_else(unavailable)?;
    let url = format!("{}/v1/lexicon-assistant/{operation}", config.base_url);
    let mut request = if body.is_some() {
        config.client.post(url)
    } else {
        config.client.get(url)
    };
    if let Some(body) = body {
        request = request
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
    }
    let mut response = request
        .bearer_auth(&config.outbound_token)
        .timeout(Duration::from_secs(36))
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > 100_000 {
            return Err(unavailable());
        }
        bytes.extend(chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

async fn run(
    state: &AppState,
    auth: &AdminAuth,
    input: &LexiconAssistantInput,
    events: Option<&tokio::sync::mpsc::Sender<LexiconAssistantEvent>>,
) -> Result<LexiconAssistantResponse, AppError> {
    let mut messages: Vec<Value> = input
        .history
        .iter()
        .map(|m| json!({"role":m.role,"content":m.content}))
        .collect();
    messages.push(json!({"role":"user","content":json!({"entry_id":input.entry_id,"base_revision":input.base_revision,"step":input.step,
        "untrusted_unsaved_content":input.local_content,"context_truncated":input.context_truncated,"question":input.question}).to_string()}));
    let mut sources: Vec<LexiconAssistantSource> = Vec::new();
    let mut usage = LexiconAssistantUsage {
        model: String::new(),
        model_calls: 0,
        tool_calls: 0,
        input_tokens: Some(0),
        output_tokens: Some(0),
    };
    for index in 0..MAX_CALLS {
        authorize(state, auth).await?;
        let allow_tools = index + 1 < MAX_CALLS && usage.tool_calls < MAX_TOOLS as u32;
        emit(events, LexiconAssistantEvent::Reset).await?;
        emit(
            events,
            LexiconAssistantEvent::Status {
                message: "正在整理建议…".into(),
            },
        )
        .await?;
        let body = json!({"messages":messages,"allow_tools":allow_tools});
        let step = if let Some(sender) = events {
            agent_stream_request(state, auth, input, body, sender).await?
        } else {
            serde_json::from_value(agent_request(state, "step", Some(body)).await?)
                .map_err(|_| unavailable())?
        };
        if step.content.chars().count() > 16000
            || step.tool_calls.len() > 3
            || step.model.len() > 100
            || step.model.is_empty()
        {
            return Err(unavailable());
        }
        usage.model_calls += 1;
        usage.model = step.model;
        usage.input_tokens = usage
            .input_tokens
            .zip(step.input_tokens)
            .and_then(|(a, b)| a.checked_add(b));
        usage.output_tokens = usage
            .output_tokens
            .zip(step.output_tokens)
            .and_then(|(a, b)| a.checked_add(b));
        if step.tool_calls.is_empty() {
            if step.content.trim().is_empty() {
                return Err(unavailable());
            }
            tracing::info!(
                event = "lexicon_assistant_answer",
                model_calls = usage.model_calls,
                tool_calls = usage.tool_calls,
                input_tokens = usage.input_tokens,
                output_tokens = usage.output_tokens
            );
            return Ok(LexiconAssistantResponse {
                entry_id: input.entry_id,
                context_id: input.context_id,
                answer: step.content,
                sources,
                usage,
                requires_human_review: true,
            });
        }
        if !allow_tools || step.tool_calls.len() + usage.tool_calls as usize > MAX_TOOLS {
            return Err(unavailable());
        }
        messages
            .push(json!({"role":"assistant","content":step.content,"tool_calls":step.tool_calls}));
        let mut seen = std::collections::HashSet::new();
        for call in &step.tool_calls {
            if call.id.is_empty()
                || call.id.len() > 100
                || !seen.insert(&call.id)
                || call.arguments.len() > 500
            {
                return Err(unavailable());
            }
            authorize(state, auth).await?;
            let message = match call.name {
                ToolName::SearchLexicon => "正在搜索智能词库…",
                ToolName::ReadLexicon => "正在读取词条内容…",
                ToolName::LookupDictionary => "正在查询内置词典…",
                ToolName::ReadFillingRules => "正在核对填写规则…",
            };
            emit(events, LexiconAssistantEvent::Reset).await?;
            emit(
                events,
                LexiconAssistantEvent::Status {
                    message: message.into(),
                },
            )
            .await?;
            let (data, source) = execute_tool(state, auth, call).await?;
            usage.tool_calls += 1;
            let source_label = source.label.clone();
            if let Some(existing) = sources.iter_mut().find(|existing| {
                existing.kind == source.kind
                    && existing.label == source.label
                    && existing.entry_id == source.entry_id
                    && existing.version == source.version
            }) {
                existing.truncated |= source.truncated;
            } else {
                sources.push(source);
            }
            messages.push(json!({"role":"tool","content":json!({"source_label":source_label,"untrusted_data":data}).to_string(),"tool_call_id":call.id}));
        }
    }
    Err(unavailable())
}

fn query_argument(arguments: &str) -> Result<String, AppError> {
    let query: QueryArgs = serde_json::from_str(arguments).map_err(|_| unavailable())?;
    let q = query.q.trim();
    if q.is_empty() || q.chars().count() > 100 || q.contains('\0') {
        return Err(unavailable());
    }
    Ok(q.to_owned())
}

async fn execute_tool(
    state: &AppState,
    auth: &AdminAuth,
    call: &ToolCall,
) -> Result<(Value, LexiconAssistantSource), AppError> {
    let (data, kind, label, id, version) = match call.name {
        ToolName::SearchLexicon => {
            let q = query_argument(&call.arguments)?;
            let query: AdminWordListQuery =
                serde_json::from_value(json!({"q":q,"page":1,"page_size":5}))
                    .map_err(|_| unavailable())?;
            let result = super::handler::service(state)
                .list(auth.subject, query, true)
                .await
                .map_err(super::handler::map_error)?;
            let words: Vec<Value> = result.words.iter().map(|w| json!({"entry_id":w.id,"label":w.presentation.label,"status":w.status,"revision":w.revision,"gloss":w.gloss,"pos":w.pos_list})).collect();
            (
                json!({"items":words,"total":result.page.total,"limit":5,"search_scope":"keyword_limited"}),
                AssistantSourceKind::SmartLexicon,
                format!("智能词库检索：{q}"),
                None,
                None,
            )
        }
        ToolName::ReadLexicon => {
            let args: EntryArgs =
                serde_json::from_str(&call.arguments).map_err(|_| unavailable())?;
            let word = super::handler::service(state)
                .get_v3(args.entry_id)
                .await
                .map_err(super::handler::map_error)?;
            (
                json!({"entry_id":word.id,"label":word.presentation.label,"revision":word.revision,"status":word.status,"forms":word.forms,"meanings":word.meanings}),
                AssistantSourceKind::SmartLexicon,
                format!("智能词库草稿：{}", word.presentation.label),
                Some(word.id),
                Some(word.revision.to_string()),
            )
        }
        ToolName::LookupDictionary => {
            let q = query_argument(&call.arguments)?;
            let normalized = super::normalization::normalize_headword(&q)
                .map_err(|_| unavailable())?
                .key;
            let repo = LexiconRepository::new(state.pool.clone());
            let term = repo
                .dictionary_term(&normalized)
                .await
                .map_err(AppError::internal)?;
            let contents = repo
                .dictionary_contents_for_terms(&[normalized])
                .await
                .map_err(AppError::internal)?;
            let version = contents
                .first()
                .map(|c| c.provider_version.clone())
                .or_else(|| term.as_ref().map(|t| t.provider_version.clone()));
            let records: Vec<Value> = contents.iter().take(12).map(|c| json!({"pos":c.pos,"senses":c.senses,"forms":c.forms,"sounds":c.sounds,"provider":c.provider_name,"version":c.provider_version})).collect();
            let headword = term.as_ref().map(|t| json!({"term":t.term,"pos":t.pos,"region":t.region_family,"provider":t.provider_name,"version":t.provider_version}));
            (
                json!({"headword":headword,"contents":records,"content_available":!contents.is_empty(),"records_total":contents.len(),"records_limited":contents.len()>12}),
                AssistantSourceKind::BuiltinDictionary,
                format!("内置词典：{q}"),
                None,
                version,
            )
        }
        ToolName::ReadFillingRules => {
            let _: EmptyArgs = serde_json::from_str(&call.arguments).map_err(|_| unavailable())?;
            let catalog = crate::catalog::service::CatalogService::new(
                crate::catalog::repository::CatalogRepository::new(state.pool.clone()),
            )
            .catalog()
            .await
            .map_err(|_| unavailable())?;
            (
                json!({"rules":RULES,"catalog":catalog}),
                AssistantSourceKind::FillingRules,
                "平台填写规则与词性配置".into(),
                None,
                Some("v1".into()),
            )
        }
    };
    let (data, truncated) = bounded_projection(data);
    Ok((
        json!({"content":data,"truncated":truncated}),
        LexiconAssistantSource {
            kind,
            label,
            entry_id: id,
            version,
            truncated,
        },
    ))
}

// Strip non-content infrastructure fields, bound every branch, and tell the model about omissions.
fn bounded_projection(value: Value) -> (Value, bool) {
    fn visit(value: Value, remaining: &mut usize, truncated: &mut bool, depth: u32) -> Value {
        if *remaining == 0 || depth > 24 {
            *truncated = true;
            return Value::Null;
        }
        *remaining -= 1;
        match value {
            Value::String(text) => {
                let limit = (*remaining).min(1500);
                let content: String = text.chars().take(limit).collect();
                *truncated |= content.len() != text.len();
                *remaining = remaining.saturating_sub(content.chars().count());
                Value::String(content)
            }
            Value::Array(items) => {
                *truncated |= items.len() > 16;
                Value::Array(
                    items
                        .into_iter()
                        .take(16)
                        .map(|v| visit(v, remaining, truncated, depth + 1))
                        .collect(),
                )
            }
            Value::Object(items) => Value::Object(
                items
                    .into_iter()
                    .filter(|(key, _)| {
                        ![
                            "audio",
                            "audio_asset",
                            "audio_assets",
                            "asset_id",
                            "audio_url",
                            "url",
                            "created_by",
                            "archived_by",
                            "synthesis",
                            "synthesis_profile",
                            "voice_id",
                            "voice_versions",
                        ]
                        .contains(&key.as_str())
                    })
                    .map(|(k, v)| (k, visit(v, remaining, truncated, depth + 1)))
                    .collect(),
            ),
            value => value,
        }
    }
    let mut truncated = false;
    let output = visit(value, &mut 12000, &mut truncated, 0);
    (output, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tools_reject_write_actions_and_extra_arguments() {
        assert!(
            serde_json::from_value::<ToolCall>(
                json!({"id":"1","name":"save_lexicon","arguments":"{}"})
            )
            .is_err()
        );
        assert!(query_argument(r#"{"q":"bank","sql":"DELETE"}"#).is_err());
        assert!(serde_json::from_str::<EntryArgs>(r#"{"entry_id":"not-a-uuid"}"#).is_err());
    }
    #[test]
    fn projection_removes_infrastructure_and_marks_missing_content() {
        let (data, truncated) = bounded_projection(
            json!({"text":"bank","url":"secret-object-url","audio":{"asset_id":"secret"},"senses":vec!["definition";20]}),
        );
        assert!(data.get("url").is_none());
        assert!(data.get("audio").is_none());
        assert_eq!(data["text"], "bank");
        assert_eq!(data["senses"].as_array().unwrap().len(), 16);
        assert!(truncated);
    }
}
