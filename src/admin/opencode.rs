//! Connect the server's OpenCode Console account without exposing its credentials.
use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize, de::IgnoredAny};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use utoipa::ToSchema;

use crate::{
    admin::{AdminAuth, authorization::require_super_admin},
    error::{AppError, ErrorCode},
    state::AppState,
};

pub struct OpenCodeConfig {
    base_url: String,
    password: String,
    client: reqwest::Client,
}

impl OpenCodeConfig {
    pub fn from_env() -> anyhow::Result<Option<Arc<Self>>> {
        let base = std::env::var("OPENCODE_SERVER_URL")
            .ok()
            .filter(|v| !v.is_empty());
        let password = std::env::var("OPENCODE_SERVER_PASSWORD")
            .ok()
            .filter(|v| !v.is_empty());
        if base.is_none() && password.is_none() {
            return Ok(None);
        }
        let base = base.ok_or_else(|| anyhow::anyhow!("OPENCODE_SERVER_URL missing"))?;
        let password =
            password.ok_or_else(|| anyhow::anyhow!("OPENCODE_SERVER_PASSWORD missing"))?;
        Ok(Some(Arc::new(Self::new(&base, password)?)))
    }

    pub fn new(base: &str, password: String) -> anyhow::Result<Self> {
        let url = reqwest::Url::parse(base).map_err(|_| anyhow::anyhow!("invalid OpenCode URL"))?;
        anyhow::ensure!(
            (url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))))
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "OpenCode URL must be an HTTPS or loopback HTTP origin"
        );
        anyhow::ensure!(
            password.len() >= 32,
            "OpenCode server password must contain at least 32 bytes"
        );
        Ok(Self {
            base_url: base.trim_end_matches('/').to_owned(),
            password,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15))
                .build()?,
        })
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, AppError> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base_url))
            .basic_auth("opencode", Some(&self.password));
        if let Some(body) = body {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(&body).map_err(|_| unavailable())?);
        }
        let mut response = request.send().await.map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(unavailable());
        }
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if bytes.len() + chunk.len() > 64_000 {
                return Err(unavailable());
            }
            bytes.extend_from_slice(&chunk);
        }
        let mut value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
        value
            .get_mut("data")
            .map(serde_json::Value::take)
            .map(Some)
            .ok_or_else(unavailable)
    }

    async fn integration(&self) -> Result<NativeIntegration, AppError> {
        let value = self
            .call(Method::GET, "/api/integration/opencode", None)
            .await?
            .ok_or_else(unavailable)?;
        let integration: NativeIntegration =
            serde_json::from_value(value).map_err(|_| unavailable())?;
        if integration.id != "opencode" {
            return Err(unavailable());
        }
        Ok(integration)
    }
}

#[derive(Deserialize)]
struct NativeIntegration {
    id: String,
    methods: Vec<NativeMethod>,
    connections: Vec<IgnoredAny>,
}

#[derive(Deserialize)]
struct NativeMethod {
    id: Option<String>,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct NativeAttempt {
    #[serde(rename = "attemptID")]
    attempt_id: String,
    url: String,
    mode: String,
    time: NativeTime,
}

#[derive(Deserialize)]
struct NativeTime {
    expires: i64,
}

#[derive(Deserialize)]
struct NativeStatus {
    status: OpenCodeLoginState,
    time: NativeTime,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeConnection {
    pub configured: bool,
    pub connected: bool,
    pub login_available: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeLogin {
    pub attempt_id: String,
    pub authorization_url: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeLoginState {
    Pending,
    Complete,
    Failed,
    Expired,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeLoginStatus {
    pub status: OpenCodeLoginState,
    pub expires_at: DateTime<Utc>,
}

fn unavailable() -> AppError {
    AppError::unavailable(
        ErrorCode::ServiceUnavailable,
        "OpenCode 连接暂时不可用，请稍后重试",
    )
}

fn valid_attempt(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

fn attempt_path(id: &str) -> Result<String, AppError> {
    if !valid_attempt(id) {
        return Err(AppError::bad_request(
            ErrorCode::InvalidPathParameter,
            "无效的登录请求",
        ));
    }
    Ok(format!("/api/integration/opencode/connect/oauth/{id}"))
}

fn no_store(value: impl Serialize) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
}

#[utoipa::path(get, path="/api/v1/admin/opencode", tag="admin", security(("bearer_auth"=[])),
    responses((status=200,body=OpenCodeConnection),(status=401,description="会话失效"),(status=403,description="仅超管可访问"),(status=503,description="OpenCode 不可用")))]
pub async fn connection(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    let Some(config) = &state.opencode else {
        return Ok(no_store(OpenCodeConnection {
            configured: false,
            connected: false,
            login_available: false,
        }));
    };
    let integration = config.integration().await?;
    Ok(no_store(OpenCodeConnection {
        configured: true,
        connected: !integration.connections.is_empty(),
        login_available: integration
            .methods
            .iter()
            .any(|method| method.kind == "oauth" && method.id.as_deref() == Some("device")),
    }))
}

#[utoipa::path(post, path="/api/v1/admin/opencode/login", tag="admin", security(("bearer_auth"=[])),
    responses((status=200,body=OpenCodeLogin),(status=401,description="会话失效"),(status=403,description="仅超管可访问"),(status=503,description="OpenCode 不可用")))]
pub async fn login(State(state): State<AppState>, auth: AdminAuth) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    let config = state.opencode.as_ref().ok_or_else(unavailable)?;
    let value = config.call(Method::POST, "/api/integration/opencode/connect/oauth", Some(json!({
        "methodID": "device", "answer": {"server": "https://opencode.ai/console"}, "label": "TSZ"
    }))).await?.ok_or_else(unavailable)?;
    let attempt: NativeAttempt = serde_json::from_value(value).map_err(|_| unavailable())?;
    let url = reqwest::Url::parse(&attempt.url).map_err(|_| unavailable())?;
    if !valid_attempt(&attempt.attempt_id)
        || attempt.mode != "auto"
        || url.scheme() != "https"
        || url.host_str() != Some("opencode.ai")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/console/device"
        || url.fragment().is_some()
    {
        if valid_attempt(&attempt.attempt_id) {
            let _ = config
                .call(Method::DELETE, &attempt_path(&attempt.attempt_id)?, None)
                .await;
        }
        return Err(unavailable());
    }
    Ok(no_store(OpenCodeLogin {
        attempt_id: attempt.attempt_id,
        authorization_url: attempt.url,
        expires_at: DateTime::from_timestamp_millis(attempt.time.expires)
            .ok_or_else(unavailable)?,
    }))
}

#[utoipa::path(get, path="/api/v1/admin/opencode/login/{attempt_id}", tag="admin", security(("bearer_auth"=[])),
    params(("attempt_id"=String,Path)),
    responses((status=200,body=OpenCodeLoginStatus),(status=400,description="无效请求"),(status=401,description="会话失效"),(status=403,description="仅超管可访问"),(status=503,description="OpenCode 不可用")))]
pub async fn login_status(
    State(state): State<AppState>,
    auth: AdminAuth,
    Path(attempt_id): Path<String>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    let config = state.opencode.as_ref().ok_or_else(unavailable)?;
    let value = config
        .call(Method::GET, &attempt_path(&attempt_id)?, None)
        .await?
        .ok_or_else(unavailable)?;
    let status: NativeStatus = serde_json::from_value(value).map_err(|_| unavailable())?;
    Ok(no_store(OpenCodeLoginStatus {
        status: status.status,
        expires_at: DateTime::from_timestamp_millis(status.time.expires).ok_or_else(unavailable)?,
    }))
}

#[utoipa::path(delete, path="/api/v1/admin/opencode/login/{attempt_id}", tag="admin", security(("bearer_auth"=[])),
    params(("attempt_id"=String,Path)),
    responses((status=204,description="登录已取消"),(status=400,description="无效请求"),(status=401,description="会话失效"),(status=403,description="仅超管可访问"),(status=503,description="OpenCode 不可用")))]
pub async fn cancel_login(
    State(state): State<AppState>,
    auth: AdminAuth,
    Path(attempt_id): Path<String>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    let config = state.opencode.as_ref().ok_or_else(unavailable)?;
    config
        .call(Method::DELETE, &attempt_path(&attempt_id)?, None)
        .await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "no-store")],
    )
        .into_response())
}
