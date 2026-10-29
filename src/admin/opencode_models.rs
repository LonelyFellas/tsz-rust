//! Model catalog, server-side credentials and the Agent's active model selection.
use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::{
    admin::{AdminAuth, authorization::require_super_admin},
    error::{AppError, ErrorCode},
    state::AppState,
};

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeModel {
    pub provider_id: String,
    pub model_id: String,
    pub name: String,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeModelList {
    pub models: Vec<OpenCodeModel>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeProvider {
    pub id: String,
    pub name: String,
    pub connected: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeProviderList {
    pub providers: Vec<OpenCodeProvider>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeModelSource {
    Direct,
    Opencode,
    Chatgpt,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeModelSelection {
    pub source: OpenCodeModelSource,
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeKeyInput {
    pub provider_id: String,
    #[schema(write_only = true, min_length = 1, max_length = 8192)]
    pub api_key: String,
}

#[derive(Deserialize)]
struct NativeModel {
    #[serde(rename = "providerID")]
    provider_id: String,
    id: String,
    name: String,
    enabled: bool,
    capabilities: NativeCapabilities,
}
#[derive(Deserialize)]
struct NativeCapabilities {
    output: Vec<String>,
}
#[derive(Deserialize)]
struct NativeProvider {
    id: String,
    name: String,
    methods: Vec<NativeMethod>,
    connections: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
struct NativeMethod {
    #[serde(rename = "type")]
    kind: String,
    form: Option<Vec<serde_json::Value>>,
}

fn unavailable() -> AppError {
    AppError::unavailable(
        ErrorCode::ServiceUnavailable,
        "模型服务暂时不可用，请稍后重试",
    )
}
fn invalid() -> AppError {
    AppError::bad_request(
        ErrorCode::InvalidRequestBody,
        "模型或 API Key 配置无效，请检查后重试",
    )
}
fn no_store(value: impl Serialize) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
}
async fn providers(state: &AppState) -> Result<Vec<NativeProvider>, AppError> {
    let config = state.opencode.as_ref().ok_or_else(unavailable)?;
    let value = config
        .call(Method::GET, "/api/integration", None)
        .await?
        .ok_or_else(unavailable)?;
    serde_json::from_value(value).map_err(|_| unavailable())
}
fn supports_key(provider: &NativeProvider) -> bool {
    provider
        .methods
        .iter()
        .any(|m| m.kind == "key" && m.form.as_ref().is_none_or(Vec::is_empty))
}

#[utoipa::path(get, path="/api/v1/admin/opencode/models", tag="admin", security(("bearer_auth"=[])),
    responses((status=200,body=OpenCodeModelList),(status=401,description="会话失效"),(status=403,description="仅超管"),(status=503,description="模型服务不可用")))]
pub async fn models(State(state): State<AppState>, auth: AdminAuth) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    let config = state.opencode.as_ref().ok_or_else(unavailable)?;
    let value = config
        .call(Method::GET, "/api/model", None)
        .await?
        .ok_or_else(unavailable)?;
    let models: Vec<NativeModel> = serde_json::from_value(value).map_err(|_| unavailable())?;
    Ok(no_store(OpenCodeModelList {
        models: models
            .into_iter()
            .filter(|m| m.capabilities.output.iter().any(|v| v == "text"))
            .map(|m| OpenCodeModel {
                provider_id: m.provider_id,
                model_id: m.id,
                name: m.name,
                enabled: m.enabled,
            })
            .collect(),
    }))
}

#[utoipa::path(get, path="/api/v1/admin/opencode/providers", tag="admin", security(("bearer_auth"=[])),
    responses((status=200,body=OpenCodeProviderList),(status=401,description="会话失效"),(status=403,description="仅超管"),(status=503,description="模型服务不可用")))]
pub async fn key_providers(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    Ok(no_store(OpenCodeProviderList {
        providers: providers(&state)
            .await?
            .into_iter()
            .filter(supports_key)
            .map(|p| OpenCodeProvider {
                id: p.id,
                name: p.name,
                connected: !p.connections.is_empty(),
            })
            .collect(),
    }))
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeConnectionMethod {
    Oauth,
    Key,
    External,
    Env,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeProviderConnection {
    pub id: Option<String>,
    pub method: OpenCodeConnectionMethod,
    pub label: String,
    pub active: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeConnectedProvider {
    pub id: String,
    pub name: String,
    pub connections: Vec<OpenCodeProviderConnection>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeConnectedProviderList {
    pub providers: Vec<OpenCodeConnectedProvider>,
}

// Only model integrations belong here; unrelated integrations must never be removed.
async fn model_providers(state: &AppState) -> Result<Vec<NativeProvider>, AppError> {
    let config = state.opencode.as_ref().ok_or_else(unavailable)?;
    let value = config
        .call(Method::GET, "/api/model", None)
        .await?
        .ok_or_else(unavailable)?;
    let models: Vec<NativeModel> = serde_json::from_value(value).map_err(|_| unavailable())?;
    Ok(providers(state)
        .await?
        .into_iter()
        .filter(|p| p.id == "opencode" || models.iter().any(|m| m.provider_id == p.id))
        .collect())
}

fn connection_info(
    c: &serde_json::Value,
    active: bool,
) -> Result<OpenCodeProviderConnection, AppError> {
    let text = |field| {
        c.get(field)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(unavailable)
    };
    let (id, method, label) = match text("type")? {
        "credential" => {
            let id = text("id")?;
            if id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(unavailable());
            }
            let method = match text("method")? {
                "oauth" => OpenCodeConnectionMethod::Oauth,
                "key" => OpenCodeConnectionMethod::Key,
                "external" => OpenCodeConnectionMethod::External,
                _ => return Err(unavailable()),
            };
            (Some(id.to_owned()), method, text("label")?.to_owned())
        }
        "env" => (
            None,
            OpenCodeConnectionMethod::Env,
            text("name")?.to_owned(),
        ),
        _ => return Err(unavailable()),
    };
    Ok(OpenCodeProviderConnection {
        id,
        method,
        label,
        active,
    })
}

#[utoipa::path(get, path="/api/v1/admin/opencode/connections", tag="admin", security(("bearer_auth"=[])),
    responses((status=200,body=OpenCodeConnectedProviderList),(status=401,description="会话失效"),(status=403,description="仅超管"),(status=503,description="模型服务不可用")))]
pub async fn connected_providers(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    let providers = model_providers(&state)
        .await?
        .into_iter()
        .filter(|p| !p.connections.is_empty())
        .map(|p| {
            let connections = p
                .connections
                .iter()
                .enumerate()
                .map(|(i, c)| connection_info(c, i == 0))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(OpenCodeConnectedProvider {
                id: p.id,
                name: p.name,
                connections,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(no_store(OpenCodeConnectedProviderList { providers }))
}

async fn change_connection(
    state: &AppState,
    provider_id: &str,
    connection_id: &str,
    activate: bool,
) -> Result<Response, AppError> {
    let provider = model_providers(state)
        .await?
        .into_iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(invalid)?;
    // Resolve the ID from this provider's public integration data, never from the secret credential listing.
    let connections = provider
        .connections
        .iter()
        .enumerate()
        .map(|(i, c)| connection_info(c, i == 0))
        .collect::<Result<Vec<_>, _>>()?;
    if !connections
        .iter()
        .any(|c| c.id.as_deref() == Some(connection_id))
    {
        return Err(invalid());
    }
    let config = state.opencode.as_ref().ok_or_else(unavailable)?;
    let path = format!(
        "/api/credential/{connection_id}{}",
        if activate { "/activate" } else { "" }
    );
    config
        .call(
            if activate {
                Method::POST
            } else {
                Method::DELETE
            },
            &path,
            None,
        )
        .await?;
    let current = providers(state)
        .await?
        .into_iter()
        .find(|p| p.id == provider_id);
    let succeeded = if activate {
        current
            .as_ref()
            .and_then(|p| p.connections.first())
            .is_some_and(|c| c["type"] == "credential" && c["id"] == connection_id)
    } else {
        current.as_ref().is_none_or(|p| {
            !p.connections
                .iter()
                .any(|c| c["type"] == "credential" && c["id"] == connection_id)
        })
    };
    if !succeeded {
        return Err(unavailable());
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "no-store")],
    )
        .into_response())
}

#[utoipa::path(delete, path="/api/v1/admin/opencode/providers/{provider_id}/connections/{connection_id}", tag="admin", security(("bearer_auth"=[])),
    params(("provider_id"=String,Path),("connection_id"=String,Path)),
    responses((status=204,description="此凭据已登出"),(status=400,description="无效连接"),(status=401,description="会话失效"),(status=403,description="仅超管"),(status=503,description="模型服务不可用")))]
pub async fn disconnect_connection(
    State(state): State<AppState>,
    auth: AdminAuth,
    Path((provider_id, connection_id)): Path<(String, String)>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    change_connection(&state, &provider_id, &connection_id, false).await
}

#[utoipa::path(post, path="/api/v1/admin/opencode/providers/{provider_id}/connections/{connection_id}/activate", tag="admin", security(("bearer_auth"=[])),
    params(("provider_id"=String,Path),("connection_id"=String,Path)),
    responses((status=204,description="已切换当前凭据"),(status=400,description="无效连接"),(status=401,description="会话失效"),(status=403,description="仅超管"),(status=503,description="模型服务不可用")))]
pub async fn activate_connection(
    State(state): State<AppState>,
    auth: AdminAuth,
    Path((provider_id, connection_id)): Path<(String, String)>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    change_connection(&state, &provider_id, &connection_id, true).await
}

#[utoipa::path(post, path="/api/v1/admin/opencode/keys", tag="admin", security(("bearer_auth"=[])), request_body=OpenCodeKeyInput,
    responses((status=204,description="凭据已保存"),(status=400,description="无效配置"),(status=401,description="会话失效"),(status=403,description="仅超管"),(status=503,description="模型服务不可用")))]
pub async fn connect_key(
    State(state): State<AppState>,
    auth: AdminAuth,
    Json(body): Json<OpenCodeKeyInput>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    if body.api_key.trim().is_empty()
        || body.api_key.len() > 8192
        || !providers(&state)
            .await?
            .iter()
            .any(|p| p.id == body.provider_id && supports_key(p))
    {
        return Err(invalid());
    }
    let config = state.opencode.as_ref().ok_or_else(unavailable)?;
    let mut url =
        reqwest::Url::parse("http://localhost/api/integration/").map_err(|_| invalid())?;
    url.path_segments_mut()
        .map_err(|_| invalid())?
        .pop_if_empty()
        .push(&body.provider_id)
        .push("connect")
        .push("key");
    config
        .call(
            Method::POST,
            url.path(),
            Some(json!({"key":body.api_key,"label":"TSZ"})),
        )
        .await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "no-store")],
    )
        .into_response())
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeProbeStatus {
    Unverified,
    Checking,
    Available,
    Unavailable,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeProbeState {
    Idle,
    Running,
    Completed,
    Interrupted,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeProbeError {
    Disabled,
    NotConfigured,
    Timeout,
    RequestFailed,
    InvalidResponse,
    Interrupted,
    RuntimeFailed,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeModelProbe {
    pub source: OpenCodeModelSource,
    pub provider_id: String,
    pub model_id: String,
    pub name: String,
    pub enabled: bool,
    pub status: OpenCodeProbeStatus,
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub duration_ms: Option<f64>,
    pub http_status: Option<u16>,
    pub error: Option<OpenCodeProbeError>,
    pub cost: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeModelProbeReport {
    pub batch_id: Option<uuid::Uuid>,
    pub state: OpenCodeProbeState,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub models: Vec<OpenCodeModelProbe>,
}

async fn probe_request(state: &AppState, start: bool) -> Result<Response, AppError> {
    let config = state.vocabulary_agent.as_ref().ok_or_else(unavailable)?;
    let mut response = config
        .client
        .request(
            if start { Method::POST } else { Method::GET },
            format!("{}/v1/model-probes", config.base_url),
        )
        .timeout(std::time::Duration::from_secs(20))
        .bearer_auth(&config.outbound_token)
        .send()
        .await
        .map_err(|_| unavailable())?;
    let expected = if start {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    if response.status() != expected {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > 2_000_000 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    let report: OpenCodeModelProbeReport =
        serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    Ok((
        expected,
        [(header::CACHE_CONTROL, "no-store")],
        Json(report),
    )
        .into_response())
}

#[utoipa::path(get, path="/api/v1/admin/opencode/probes", tag="admin", security(("bearer_auth"=[])),
 responses((status=200,body=OpenCodeModelProbeReport),(status=401,description="会话失效"),(status=403,description="仅超管"),(status=503,description="探针服务不可用")))]
pub async fn probe_report(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    probe_request(&state, false).await
}

#[utoipa::path(post, path="/api/v1/admin/opencode/probes", tag="admin", security(("bearer_auth"=[])),
 responses((status=202,body=OpenCodeModelProbeReport),(status=401,description="会话失效"),(status=403,description="仅超管"),(status=503,description="探针服务不可用")))]
pub async fn start_probes(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    probe_request(&state, true).await
}

async fn selection(
    state: &AppState,
    body: Option<OpenCodeModelSelection>,
) -> Result<Response, AppError> {
    let config = state.vocabulary_agent.as_ref().ok_or_else(unavailable)?;
    let url = format!("{}/v1/model-selection", config.base_url);
    let mut request = config
        .client
        .request(
            if body.is_some() {
                Method::PUT
            } else {
                Method::GET
            },
            url,
        )
        .timeout(std::time::Duration::from_secs(20))
        .bearer_auth(&config.outbound_token);
    if let Some(body) = body {
        request = request
            .header(header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&body).map_err(|_| invalid())?);
    }
    let mut response = request.send().await.map_err(|_| unavailable())?;
    if response.status() == StatusCode::UNPROCESSABLE_ENTITY {
        return Err(invalid());
    }
    if response.status() != StatusCode::OK {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > 8192 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: OpenCodeModelSelection =
        serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    Ok(no_store(value))
}

#[utoipa::path(get, path="/api/v1/admin/opencode/model", tag="admin", security(("bearer_auth"=[])),
    responses((status=200,body=OpenCodeModelSelection),(status=401,description="会话失效"),(status=403,description="仅超管"),(status=503,description="Agent 不可用")))]
pub async fn current_model(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    selection(&state, None).await
}

#[utoipa::path(put, path="/api/v1/admin/opencode/model", tag="admin", security(("bearer_auth"=[])), request_body=OpenCodeModelSelection,
    responses((status=200,body=OpenCodeModelSelection),(status=400,description="模型不可用"),(status=401,description="会话失效"),(status=403,description="仅超管"),(status=503,description="Agent 不可用")))]
pub async fn select_model(
    State(state): State<AppState>,
    auth: AdminAuth,
    Json(body): Json<OpenCodeModelSelection>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    selection(&state, Some(body)).await
}
