//! Read-only assistant: the model proposes tools; the authenticated API executes them.
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header},
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
    lexicon::dto::{AdminWordListQuery, WordCreationStep},
    state::AppState,
};

// Prepared materials answer most questions in one call; tools remain for other words.
const MAX_CALLS: u32 = 3;
const MAX_TOOLS: usize = 6;
#[path = "assistant_candidates.rs"]
pub mod candidates;
#[path = "assistant_materials.rs"]
mod materials;
/// Filling rules live in tsz-agent's system prompt; only older Agents still ask for them.
const RULES_NOTICE: &str = "填写规则由 Agent 系统说明提供。";
const OFFERED_TOOLS: [&str; 4] = [
    "check_draft",
    "find_entries",
    "read_lexicon",
    "lookup_dictionary",
];
const EXTENSIONS_HEADER: &str = "x-tsz-assistant-version";
const MAX_DRAFT_BYTES: usize = 1_500_000;
/// Agent rejects longer message contents; leave room for history and JSON escaping.
const MAX_USER_MESSAGE_CHARS: usize = 45_000;
const AUTHORIZATION_RECHECK: Duration = Duration::from_secs(3);
const SAVED_DURING_ANSWER: &str =
    "\n\n（提示：回答期间词条已保存过，以上内容基于提问时的版本，请按最新内容核对。）";

/// Structured unsaved editor content, in the same shapes the step save endpoints accept.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconAssistantDraft {
    #[schema(value_type = crate::lexicon::dto::FormsStepRepresentationV3)]
    pub forms: Value,
    #[schema(value_type = crate::lexicon::dto::DraftMeaningsStepContentWritableV3)]
    pub meanings: Value,
}

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
    /// Newer editors send structured content; the server validates and projects it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub draft: Option<LexiconAssistantDraft>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub extensions: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_refs: Option<Vec<LexiconAssistantFieldRef>>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconAssistantFieldRef {
    pub path: String,
    pub label: String,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconAssistantFeatureSet {
    pub structured_draft: bool,
    pub history_summary: bool,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconAssistantCapability {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<LexiconAssistantFeatureSet>,
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
            let mut output = run(&state, &auth, &input, Some(&sender)).await?;
            authorize(&state, &auth).await?;
            // A save while answering does not invalidate advice bound to the asked snapshot.
            if revision_changed(&state, &input).await? {
                emit(
                    Some(&sender),
                    LexiconAssistantEvent::Delta {
                        text: SAVED_DURING_ANSWER.into(),
                    },
                )
                .await?;
                output.answer.push_str(SAVED_DURING_ANSWER);
            }
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
    body: Value,
    sender: &tokio::sync::mpsc::Sender<LexiconAssistantEvent>,
) -> Result<ModelStep, AppError> {
    let config = state.vocabulary_agent.as_ref().ok_or_else(unavailable)?;
    let mut authorized_at = std::time::Instant::now();
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
                    // Revocation still stops delivery within a few seconds without a
                    // permission reload for every 16-character delta.
                    if authorized_at.elapsed() >= AUTHORIZATION_RECHECK {
                        authorize(state, auth).await?;
                        authorized_at = std::time::Instant::now();
                    }
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
    FindEntries,
    CheckDraft,
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
struct SpellingArgs {
    spelling: String,
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
    if let Some(draft) = &input.draft
        && serde_json::to_vec(draft).map_or(true, |bytes| bytes.len() > MAX_DRAFT_BYTES)
    {
        return Err(invalid("当前编辑内容过大，请缩小问题范围后重试"));
    }
    Ok(())
}

#[utoipa::path(get,path="/api/v1/admin/lexicon/assistant/capability",tag="admin-lexicon",security(("bearer_auth"=[])),
    responses((status=200,body=LexiconAssistantCapability,description="词库助手配置状态"),(status=401,description="身份失效"),(status=403,description="缺少 words.access")))]
pub async fn capability(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    let available = if state.vocabulary_agent.is_some() {
        agent_request(&state, "capability", None)
            .await
            .ok()
            .and_then(|v| v["available"].as_bool())
            .unwrap_or(false)
    } else {
        false
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(LexiconAssistantCapability {
            available,
            features: (headers.get(EXTENSIONS_HEADER).and_then(|v| v.to_str().ok()) == Some("2"))
                .then_some(LexiconAssistantFeatureSet {
                    structured_draft: true,
                    history_summary: true,
                }),
        }),
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
    let mut output = result?;
    authorize(&state, &auth).await?;
    if revision_changed(&state, &input).await? {
        output.answer.push_str(SAVED_DURING_ANSWER);
    }
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

async fn revision_changed(
    state: &AppState,
    input: &LexiconAssistantInput,
) -> Result<bool, AppError> {
    match check_revision(state, input).await {
        Ok(()) => Ok(false),
        Err(error) if error.status_code() == axum::http::StatusCode::CONFLICT => Ok(true),
        Err(error) => Err(error),
    }
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
    if operation == "capability" {
        request = request.header(EXTENSIONS_HEADER, "2");
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
    let mut sources: Vec<LexiconAssistantSource> = Vec::new();
    let mut tool_summaries = Vec::<Value>::new();
    let prepared = prepare(state, input, &mut sources).await?;
    let prepared_value: Value = serde_json::from_str(&prepared).map_err(|_| unavailable())?;
    let agent_capability = agent_request(state, "capability", None).await.ok();
    let supports_tools = agent_capability
        .as_ref()
        .and_then(|v| v["tool_selection"].as_bool())
        .unwrap_or(false);
    if let Some(version) = agent_capability
        .as_ref()
        .and_then(|v| v["rules_version"].as_str())
    {
        add_source(
            &mut sources,
            LexiconAssistantSource {
                kind: AssistantSourceKind::FillingRules,
                label: "填写规则".into(),
                entry_id: None,
                version: Some(version.into()),
                truncated: false,
            },
        );
    }
    messages.push(json!({"role":"user","content":prepared}));
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
        let mut body = json!({"messages":messages,"allow_tools":allow_tools});
        if supports_tools {
            body["tools"] = json!(OFFERED_TOOLS);
        }
        if body.to_string().len() > 390_000 {
            return Err(invalid("查询资料过多，请缩小问题范围后重试"));
        }
        let step = if let Some(sender) = events {
            agent_stream_request(state, auth, body, sender).await?
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
                context_summary: input.extensions.then(|| {
                    let mut summary = prepared_value["server_materials"].clone();
                    summary["catalog"] = Value::Null;
                    summary["tool_results"] = json!(tool_summaries);
                    let (summary, _) = bounded_projection_with(summary, 2800);
                    summary.to_string()
                }),
                field_refs: input.extensions.then(|| {
                    prepared_value["server_materials"]["validation"]["result"]["issues"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|i| serde_json::from_value(i["location"].clone()).ok())
                        .collect()
                }),
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
                ToolName::FindEntries => "正在查找同拼写词条…",
                ToolName::CheckDraft => "正在核对本次提问的草稿…",
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
            let (data, source) = if matches!(call.name, ToolName::CheckDraft) {
                let _: EmptyArgs =
                    serde_json::from_str(&call.arguments).map_err(|_| unavailable())?;
                (
                    prepared_value["server_materials"]["validation"].clone(),
                    LexiconAssistantSource {
                        kind: AssistantSourceKind::FillingRules,
                        label: "当前草稿校验".into(),
                        entry_id: None,
                        version: None,
                        truncated: false,
                    },
                )
            } else {
                execute_tool(state, auth, call).await?
            };
            usage.tool_calls += 1;
            let source_label = source.label.clone();
            let (summary, _) = bounded_projection_with(data.clone(), 1000);
            tool_summaries.push(json!({"source":source_label,"data":summary}));
            add_source(&mut sources, source);
            messages.push(json!({"role":"tool","content":json!({"source_label":source_label,"untrusted_data":data}).to_string(),"tool_call_id":call.id}));
        }
    }
    Err(unavailable())
}

fn add_source(sources: &mut Vec<LexiconAssistantSource>, source: LexiconAssistantSource) {
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
}

async fn catalog(state: &AppState) -> Result<crate::catalog::model::CatalogResponse, AppError> {
    crate::catalog::service::CatalogService::new(
        crate::catalog::repository::CatalogRepository::new(state.pool.clone()),
    )
    .catalog()
    .await
    .map_err(|_| unavailable())
}

/// Materials the model should not have to ask for: publish-rule issues for the asked
/// snapshot, the headword's built-in dictionary record, same-spelling entries and the
/// part-of-speech catalog. Each part is bounded; a failed lookup is reported, not fatal.
async fn prepare(
    state: &AppState,
    input: &LexiconAssistantInput,
    sources: &mut Vec<LexiconAssistantSource>,
) -> Result<String, AppError> {
    let saved = super::handler::service(state)
        .get_v3(input.entry_id)
        .await
        .map_err(super::handler::map_error)?;
    let draft = input
        .draft
        .as_ref()
        .and_then(|draft| materials::parse_draft(&draft.forms, &draft.meanings));
    if input.draft.is_some() && draft.is_none() {
        return Err(invalid("当前编辑结构无效，请核对草稿后重试"));
    }
    let snapshot = materials::Draft {
        forms: saved.forms.clone(),
        meanings: saved.meanings.clone(),
    };
    let current = draft.as_ref().unwrap_or(&snapshot);
    let mut headwords = materials::headwords(&current.forms);
    if headwords.is_empty() {
        let initial: Option<Value> = sqlx::query_scalar(
            "SELECT initial_headwords FROM lexicon.v3_entry_state WHERE entry_id=$1",
        )
        .bind(input.entry_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::internal)?
        .flatten();
        if let Some(initial) = initial {
            for key in ["common", "uk", "us"] {
                if let Some(spelling) = initial[key].as_str()
                    && !headwords.iter().any(|s| s == spelling)
                {
                    headwords.push(spelling.into());
                }
            }
        }
    }
    let headword = headwords.join(" / ");
    let catalog_result = catalog(state).await.ok();
    let scope = if draft.is_some() {
        "当前未保存编辑"
    } else {
        "已保存版本（未包含未保存编辑）"
    };
    let validation = if let Some(catalog) = &catalog_result {
        let parents = catalog
            .items
            .iter()
            .flat_map(|p| p.sub_parts.iter().map(|s| (s.code.clone(), p.code.clone())))
            .collect();
        materials::check(input.entry_id, current, &parents)
    } else {
        json!({"available":false,"note":"词性目录未取到，无法完成草稿校验"})
    };
    let mut dictionaries = Vec::new();
    let mut same_spellings = Vec::new();
    for headword in headwords.iter().take(8) {
        let dictionary = match materials::dictionary(state, headword).await {
            Ok((data, version)) => {
                let (data, truncated) = bounded_projection_with(data, 4000);
                add_source(
                    sources,
                    LexiconAssistantSource {
                        kind: AssistantSourceKind::BuiltinDictionary,
                        label: format!("内置词典：{headword}"),
                        entry_id: None,
                        version,
                        truncated,
                    },
                );
                data
            }
            Err(_) => json!({"available": false}),
        };
        let same_spelling =
            match materials::same_spelling(state, headword, Some(input.entry_id)).await {
                Ok(data) => {
                    add_source(
                        sources,
                        LexiconAssistantSource {
                            kind: AssistantSourceKind::SmartLexicon,
                            label: format!("库内同拼写：{headword}"),
                            entry_id: None,
                            version: None,
                            truncated: data["limited"].as_bool().unwrap_or(false),
                        },
                    );
                    data
                }
                Err(_) => json!({"available": false}),
            };
        dictionaries.push(json!({"spelling":headword,"data":dictionary}));
        same_spellings.push(same_spelling);
    }
    let dictionary = json!(dictionaries);
    let same_spelling = json!(same_spellings);
    let catalog = match catalog_result {
        Some(catalog) => {
            let version = catalog.catalog_version.to_string();
            let parts:Vec<Value>=catalog.items.iter().filter(|p|current.forms.pos.is_empty() || current.forms.pos.iter().any(|pos|pos.pos==p.code)).map(|p|json!({"code":p.code,"name":p.name_zh,"allowed_form_types":p.allowed_form_types,"sub_parts":p.sub_parts.iter().map(|s|json!({"code":s.code,"name":s.name_zh})).collect::<Vec<_>>()})).collect();
            let available_pos: Vec<Value> = catalog
                .items
                .iter()
                .filter(|part| {
                    serde_json::to_value(part.kind).ok() == serde_json::to_value(saved.kind).ok()
                })
                .map(|part| json!({"code":part.code,"name":part.name_zh}))
                .collect();
            let data = json!({"scope":"current_pos_details","note":"items 仅详列当前草稿已有词性；可新增词性以 available_pos 为准，不能把 items 当作完整目录", "available_pos":available_pos,"items":parts});
            let truncated = false;
            add_source(
                sources,
                LexiconAssistantSource {
                    kind: AssistantSourceKind::FillingRules,
                    label: "发布校验规则与词性配置".into(),
                    entry_id: None,
                    version: Some(version),
                    truncated,
                },
            );
            data
        }
        None => json!({"available": false}),
    };
    let mut budget = 18_000;
    loop {
        let content = match &draft {
            Some(draft) => materials::draft_view(draft, input.step, budget),
            None => Value::String(input.local_content.clone()),
        };
        let message = json!({
            "entry_id": input.entry_id,
            "base_revision": input.base_revision,
            "headword": headword,
            "step": input.step,
            "question": input.question,
            "server_materials": {
                "validation": { "scope": scope, "result": validation,
                    "note": "与发布校验相同的结构规则；引用其他词条的检查在发布时由系统完成" },
                "builtin_dictionary": dictionary,
                "same_spelling_entries": same_spelling,
                "catalog": catalog,
            },
            "untrusted_unsaved_content": content,
            "context_truncated": input.context_truncated,
        })
        .to_string();
        if message.chars().count() <= MAX_USER_MESSAGE_CHARS {
            return Ok(message);
        }
        if draft.is_none() || budget < 2_000 {
            return Err(invalid("当前编辑内容过多，请缩小问题范围后重试"));
        }
        budget /= 2;
    }
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
                {
                    let draft = materials::Draft {
                        forms: word.forms,
                        meanings: word.meanings,
                    };
                    let mut data = materials::draft_view(&draft, WordCreationStep::Meanings, 10000);
                    data["entry_id"] = json!(word.id);
                    data["label"] = json!(word.presentation.label);
                    data["revision"] = json!(word.revision);
                    data["status"] = json!(word.status);
                    data
                },
                AssistantSourceKind::SmartLexicon,
                format!("智能词库草稿：{}", word.presentation.label),
                Some(word.id),
                Some(word.revision.to_string()),
            )
        }
        ToolName::LookupDictionary => {
            let q = query_argument(&call.arguments)?;
            let (data, version) = materials::dictionary(state, &q).await?;
            (
                data,
                AssistantSourceKind::BuiltinDictionary,
                format!("内置词典：{q}"),
                None,
                version,
            )
        }
        ToolName::FindEntries => {
            let args: SpellingArgs =
                serde_json::from_str(&call.arguments).map_err(|_| unavailable())?;
            let spelling = args.spelling.trim();
            if spelling.is_empty() || spelling.chars().count() > 100 || spelling.contains('\0') {
                return Err(unavailable());
            }
            (
                materials::same_spelling(state, spelling, None).await?,
                AssistantSourceKind::SmartLexicon,
                format!("库内同拼写：{spelling}"),
                None,
                None,
            )
        }
        ToolName::CheckDraft => return Err(unavailable()),
        ToolName::ReadFillingRules => {
            let _: EmptyArgs = serde_json::from_str(&call.arguments).map_err(|_| unavailable())?;
            let catalog = catalog(state).await?;
            let version = catalog.catalog_version.to_string();
            (
                json!({"rules":RULES_NOTICE,"catalog":catalog}),
                AssistantSourceKind::FillingRules,
                "词性配置".into(),
                None,
                Some(version),
            )
        }
    };
    let (data, truncated) = if matches!(call.name, ToolName::ReadLexicon) {
        let truncated = data["omissions"]["total"].as_u64().unwrap_or(0) > 0;
        (data, truncated)
    } else {
        bounded_projection(data)
    };
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
    bounded_projection_with(value, 12000)
}

fn bounded_projection_with(value: Value, budget: usize) -> (Value, bool) {
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
    let output = visit(value, &mut budget.clone(), &mut truncated, 0);
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
