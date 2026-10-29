//! Super-admin projection of the local Agent's ChatGPT account authorization.
use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    admin::{AdminAuth, authorization::require_super_admin},
    error::{AppError, ErrorCode},
    state::AppState,
};

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatGPTAccount {
    pub client_id: String,
    pub email: String,
    pub signed_in: bool,
    pub plan_enabled: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatGPTAccounts {
    pub configured: bool,
    pub active: Option<String>,
    pub accounts: Vec<ChatGPTAccount>,
    pub pending_login: Option<ChatGPTLogin>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatGPTAccountInput {
    pub client_id: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatGPTLogin {
    pub attempt_id: Uuid,
    pub authorization_url: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatGPTLoginState {
    Pending,
    Complete,
    Failed,
    Expired,
    Cancelled,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatGPTLoginError {
    Denied,
    IdentityInvalid,
    Unavailable,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatGPTLoginStatus {
    pub status: ChatGPTLoginState,
    pub expires_at: DateTime<Utc>,
    pub error: Option<ChatGPTLoginError>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatGPTLogout {
    pub remote_revoked: bool,
}

fn unavailable() -> AppError {
    AppError::unavailable(
        ErrorCode::ServiceUnavailable,
        "ChatGPT 账号服务暂时不可用，请刷新重试",
    )
}

fn invalid() -> AppError {
    AppError::bad_request(
        ErrorCode::InvalidRequestBody,
        "ChatGPT 账号操作未完成，请检查账号状态或取消正在进行的登录后重试",
    )
}

fn no_store(value: impl Serialize) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
}

fn valid_client_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}

async fn call<T: DeserializeOwned>(
    state: &AppState,
    method: Method,
    path: &str,
    body: Option<ChatGPTAccountInput>,
) -> Result<T, AppError> {
    let config = state.vocabulary_agent.as_ref().ok_or_else(unavailable)?;
    let mut request = config
        .client
        .request(method, format!("{}{path}", config.base_url))
        .timeout(std::time::Duration::from_secs(20))
        .bearer_auth(&config.outbound_token);
    if let Some(body) = body {
        if body
            .client_id
            .as_ref()
            .is_some_and(|id| !valid_client_id(id))
        {
            return Err(invalid());
        }
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
        if bytes.len() + chunk.len() > 64_000 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

fn valid_authorization_url(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    if url.origin().ascii_serialization() != "https://auth.openai.com"
        || url.path() != "/api/accounts/authorize"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let pairs: Vec<_> = url.query_pairs().collect();
    if pairs.iter().any(|(key, _)| {
        matches!(
            key.as_ref(),
            "access_token" | "refresh_token" | "id_token" | "id_token_hint" | "client_secret"
        )
    }) {
        return false;
    }
    let redirects: Vec<_> = pairs
        .iter()
        .filter(|(key, _)| key == "redirect_uri")
        .collect();
    redirects.len() == 1
        && reqwest::Url::parse(&redirects[0].1).is_ok_and(|callback| {
            callback.scheme() == "http"
                && callback.host_str() == Some("127.0.0.1")
                && callback.path() == "/auth/callback"
                && callback.port().is_some()
                && callback.username().is_empty()
                && callback.password().is_none()
                && callback.query().is_none()
                && callback.fragment().is_none()
        })
}

#[utoipa::path(get, path="/api/v1/admin/chatgpt", tag="admin", security(("bearer_auth"=[])),
    responses((status=200,body=ChatGPTAccounts),(status=401,description="会话失效"),(status=403,description="仅超管可访问"),(status=503,description="账号服务不可用")))]
pub async fn accounts(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    if state.vocabulary_agent.is_none() {
        return Ok(no_store(ChatGPTAccounts {
            configured: false,
            active: None,
            accounts: vec![],
            pending_login: None,
        }));
    }
    let result: ChatGPTAccounts = call(&state, Method::GET, "/v1/chatgpt/accounts", None).await?;
    if result
        .pending_login
        .as_ref()
        .is_some_and(|pending| !valid_authorization_url(&pending.authorization_url))
    {
        return Err(unavailable());
    }
    Ok(no_store(result))
}

#[utoipa::path(post, path="/api/v1/admin/chatgpt/login", tag="admin", security(("bearer_auth"=[])), request_body=ChatGPTAccountInput,
    responses((status=200,body=ChatGPTLogin),(status=400,description="登录未就绪"),(status=401,description="会话失效"),(status=403,description="仅超管可访问"),(status=503,description="账号服务不可用")))]
pub async fn login(
    State(state): State<AppState>,
    auth: AdminAuth,
    Json(body): Json<ChatGPTAccountInput>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    let result: ChatGPTLogin = call(&state, Method::POST, "/v1/chatgpt/login", Some(body)).await?;
    if !valid_authorization_url(&result.authorization_url) {
        let _: Result<ChatGPTLoginStatus, _> = call(
            &state,
            Method::DELETE,
            &format!("/v1/chatgpt/login/{}", result.attempt_id),
            None,
        )
        .await;
        return Err(unavailable());
    }
    Ok(no_store(result))
}

#[utoipa::path(get, path="/api/v1/admin/chatgpt/login/{attempt_id}", tag="admin", security(("bearer_auth"=[])), params(("attempt_id"=Uuid,Path)),
    responses((status=200,body=ChatGPTLoginStatus),(status=401,description="会话失效"),(status=403,description="仅超管可访问"),(status=503,description="账号服务不可用")))]
pub async fn login_status(
    State(state): State<AppState>,
    auth: AdminAuth,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    Ok(no_store(
        call::<ChatGPTLoginStatus>(
            &state,
            Method::GET,
            &format!("/v1/chatgpt/login/{id}"),
            None,
        )
        .await?,
    ))
}

#[utoipa::path(delete, path="/api/v1/admin/chatgpt/login/{attempt_id}", tag="admin", security(("bearer_auth"=[])), params(("attempt_id"=Uuid,Path)),
    responses((status=200,body=ChatGPTLoginStatus),(status=401,description="会话失效"),(status=403,description="仅超管可访问"),(status=503,description="账号服务不可用")))]
pub async fn cancel_login(
    State(state): State<AppState>,
    auth: AdminAuth,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    Ok(no_store(
        call::<ChatGPTLoginStatus>(
            &state,
            Method::DELETE,
            &format!("/v1/chatgpt/login/{id}"),
            None,
        )
        .await?,
    ))
}

#[utoipa::path(put, path="/api/v1/admin/chatgpt/account", tag="admin", security(("bearer_auth"=[])), request_body=ChatGPTAccountInput,
    responses((status=200,body=ChatGPTAccounts),(status=400,description="账号无效"),(status=401,description="会话失效"),(status=403,description="仅超管可访问"),(status=503,description="账号服务不可用")))]
pub async fn use_account(
    State(state): State<AppState>,
    auth: AdminAuth,
    Json(body): Json<ChatGPTAccountInput>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    Ok(no_store(
        call::<ChatGPTAccounts>(&state, Method::PUT, "/v1/chatgpt/account", Some(body)).await?,
    ))
}

#[utoipa::path(delete, path="/api/v1/admin/chatgpt/accounts/{client_id}", tag="admin", security(("bearer_auth"=[])), params(("client_id"=String,Path)),
    responses((status=200,body=ChatGPTLogout),(status=400,description="账号无效"),(status=401,description="会话失效"),(status=403,description="仅超管可访问"),(status=503,description="账号服务不可用")))]
pub async fn logout(
    State(state): State<AppState>,
    auth: AdminAuth,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    if !valid_client_id(&id) {
        return Err(invalid());
    }
    Ok(no_store(
        call::<ChatGPTLogout>(
            &state,
            Method::DELETE,
            &format!("/v1/chatgpt/accounts/{id}"),
            None,
        )
        .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_is_only_the_official_loopback_flow() {
        let valid = "https://auth.openai.com/api/accounts/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback";
        assert!(valid_authorization_url(valid));
        for invalid in [
            valid.replace("auth.openai.com", "auth.openai.com.evil.test"),
            valid.replace("127.0.0.1", "example.com"),
            valid.replace("https://", "http://"),
            format!("{valid}&access_token=secret"),
        ] {
            assert!(!valid_authorization_url(&invalid));
        }
    }
}
