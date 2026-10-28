//! Super-admin-only projection of the Agent's model call ledger.
use axum::{
    Json,
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    admin::{AdminAuth, authorization::require_super_admin},
    api::ApiQuery,
    error::{AppError, ErrorCode},
    state::AppState,
};

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct AiLedgerQuery {
    #[param(minimum = 1, maximum = 1_000_000)]
    pub page: Option<u32>,
    #[param(minimum = 1, maximum = 100)]
    pub page_size: Option<u32>,
    pub run_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiCallPhase {
    Generate,
    Review,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiCallOutcome {
    Response,
    Error,
    Cancelled,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AiCallMetrics {
    pub phase: AiCallPhase,
    pub outcome: AiCallOutcome,
    pub duration_ms: f64,
    #[schema(required = true)]
    pub cache_hit_tokens: Option<u64>,
    #[schema(required = true)]
    pub cache_miss_tokens: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AiLedgerItem {
    pub call_id: Uuid,
    pub run_id: Uuid,
    pub external_request_id: Uuid,
    pub model: String,
    pub created_at: DateTime<Utc>,
    #[schema(required = true)]
    pub input_tokens: Option<u64>,
    #[schema(required = true)]
    pub output_tokens: Option<u64>,
    #[schema(required = true)]
    pub cost: Option<String>,
    #[schema(required = true)]
    pub metrics: Option<AiCallMetrics>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AiLedgerSummary {
    pub observed_calls: u64,
    pub usage_observed_calls: u64,
    pub cache_observed_calls: u64,
    #[schema(required = true)]
    pub input_tokens: Option<u64>,
    #[schema(required = true)]
    pub output_tokens: Option<u64>,
    #[schema(required = true)]
    pub cache_hit_tokens: Option<u64>,
    #[schema(required = true)]
    pub cache_miss_tokens: Option<u64>,
    #[schema(required = true)]
    pub cache_hit_rate: Option<f64>,
    #[schema(required = true)]
    pub total_duration_ms: Option<f64>,
    #[schema(required = true)]
    pub average_duration_ms: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AiLedgerPage {
    pub items: Vec<AiLedgerItem>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
    pub summary: AiLedgerSummary,
}

fn unavailable() -> AppError {
    AppError::unavailable(
        ErrorCode::ServiceUnavailable,
        "AI 账本暂时不可用，请稍后重试",
    )
}

#[utoipa::path(
    get, path = "/api/v1/admin/ai-ledger", tag = "admin", security(("bearer_auth" = [])),
    params(AiLedgerQuery),
    responses(
        (status = 200, description = "超管专属 AI 调用账本；汇总覆盖全部筛选结果", body = AiLedgerPage),
        (status = 400, description = "查询参数无效"),
        (status = 401, description = "会话失效"),
        (status = 403, description = "仅超管可访问"),
        (status = 503, description = "Agent 账本不可用")
    )
)]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<AiLedgerQuery>,
) -> Result<Response, AppError> {
    require_super_admin(&state, &auth).await?;
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);
    if !(1..=1_000_000).contains(&page) || !(1..=100).contains(&page_size) {
        return Err(AppError::bad_request(
            ErrorCode::InvalidQuery,
            "分页参数无效",
        ));
    }
    let config = state.vocabulary_agent.as_ref().ok_or_else(unavailable)?;
    let mut url = reqwest::Url::parse(&format!("{}/v1/model-calls", config.base_url))
        .map_err(|_| unavailable())?;
    {
        let mut params = url.query_pairs_mut();
        params.append_pair("page", &page.to_string());
        params.append_pair("page_size", &page_size.to_string());
        if let Some(id) = query.run_id {
            params.append_pair("run_id", &id.to_string());
        }
    }
    let mut response = config
        .client
        .get(url)
        .bearer_auth(&config.outbound_token)
        .send()
        .await
        .map_err(|_| unavailable())?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > 1_000_000 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    // Typed projection never forwards arbitrary upstream fields or error bodies.
    let result: AiLedgerPage = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    if result.page != page
        || result.page_size != page_size
        || result.items.len() > page_size as usize
    {
        return Err(unavailable());
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(result)).into_response())
}
