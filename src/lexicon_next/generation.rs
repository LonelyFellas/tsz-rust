use super::{language, model::NextListQuery, repository};
use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiJson, ApiPath},
    catalog::{repository::CatalogRepository, service::CatalogService},
    error::{AppError, ErrorCode},
    lexicon::assistant::candidates::AssistantCandidatePos,
    state::AppState,
};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NextAgentSection {
    Forms,
    Senses,
    Groups,
    Grammar,
    Definitions,
    Equivalents,
    Relations,
    Dialect,
    Examples,
    Translations,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentSenseContext {
    pub pos: String,
    pub gloss: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentInput {
    pub run_id: Uuid,
    pub base_revision: i64,
    pub snapshot_id: String,
    pub headword: String,
    pub target: String,
    pub sections: Vec<NextAgentSection>,
    pub existing_pos: Vec<String>,
    pub examples: Vec<String>,
    pub senses: Vec<NextAgentSenseContext>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentSense {
    pub pos: String,
    pub gloss: String,
    pub level: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentGroup {
    pub sense: usize,
    pub name_en: String,
    pub name_zh: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentGrammar {
    pub pos: String,
    pub pattern: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentDefinition {
    pub sense: usize,
    pub english: String,
    pub explanation: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentEquivalent {
    pub sense: usize,
    pub word: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentRelation {
    pub sense: usize,
    pub kind: String,
    pub word: String,
    pub gloss: String,
    pub score: f64,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentDialect {
    pub sense: usize,
    pub usage: super::dialect_pairs::NextDialectUsage,
    pub source_locator: String,
    pub evidence: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentExample {
    pub sense: usize,
    pub text: String,
    pub level: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentTranslation {
    pub example: usize,
    pub text: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentSource {
    pub source: String,
    pub locator: String,
    pub excerpt: String,
    pub status: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentStep {
    pub section: NextAgentSection,
    pub state: String,
    pub error: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentProposals {
    pub forms: Vec<AssistantCandidatePos>,
    pub senses: Vec<NextAgentSense>,
    pub groups: Vec<NextAgentGroup>,
    pub grammar: Vec<NextAgentGrammar>,
    pub definitions: Vec<NextAgentDefinition>,
    pub equivalents: Vec<NextAgentEquivalent>,
    pub relations: Vec<NextAgentRelation>,
    pub examples: Vec<NextAgentExample>,
    pub dialect: Vec<NextAgentDialect>,
    pub translations: Vec<NextAgentTranslation>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAgentResult {
    pub run_id: Uuid,
    pub base_revision: i64,
    pub snapshot_id: String,
    pub done: bool,
    pub requires_human_review: bool,
    pub steps: Vec<NextAgentStep>,
    pub proposals: NextAgentProposals,
    pub sources: Vec<NextAgentSource>,
}

fn unavailable() -> AppError {
    AppError::unavailable(
        ErrorCode::ServiceUnavailable,
        "词库生成服务暂不可用，草稿已保留",
    )
}

pub async fn available(state: &AppState) -> bool {
    let Some(config) = &state.vocabulary_agent else {
        return false;
    };
    let Ok(response) = config
        .client
        .get(format!("{}/v2/lexicon/capability", config.base_url))
        .bearer_auth(&config.outbound_token)
        .timeout(Duration::from_secs(2))
        .send()
        .await
    else {
        return false;
    };
    response.status().is_success()
        && response
            .bytes()
            .await
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v["generation"].as_bool())
            == Some(true)
}

async fn editable(
    state: &AppState,
    auth: &AdminAuth,
    id: Uuid,
    revision: i64,
) -> Result<(String, String), AppError> {
    let access = permissions::reload(state, auth).await?;
    access.require("words.access")?;
    let (owner,status,current,kind,medium):(Uuid,String,i64,String,String)=sqlx::query_as("SELECT owner_id,status,revision,kind,medium_locale FROM lexicon_next.entries WHERE id=$1")
        .bind(id).fetch_optional(&state.pool).await.map_err(AppError::internal)?.ok_or_else(||AppError::not_found("entry not found"))?;
    access.require_owned_action("words.edit", owner)?;
    if status == "archived" {
        return Err(AppError::conflict(
            ErrorCode::EntryArchived,
            None,
            "归档词条不能生成",
        ));
    }
    if revision != current {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "词条版本已变化，请重新生成",
        ));
    }
    Ok((kind, medium))
}

#[utoipa::path(post,path="/api/v1/admin/lexicon-next/entries/{id}/generation",tag="admin-lexicon-next",security(("bearer_auth"=[])),
    params(("id"=Uuid,Path)),request_body=NextAgentInput,responses((status=200,body=NextAgentResult),(status=401),(status=403),(status=404),(status=409),(status=422),(status=429),(status=503)))]
pub async fn advance(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<NextAgentInput>,
) -> Result<Json<NextAgentResult>, AppError> {
    let (kind, medium) = editable(&state, &auth, id, input.base_revision).await?;
    let config = state.vocabulary_agent.as_ref().ok_or_else(unavailable)?;
    language::text(&input.headword, 200)?;
    if input.run_id.is_nil()
        || input.headword.trim().is_empty()
        || !matches!(input.target.as_str(), "en-GB" | "en-US")
        || input.sections.is_empty()
        || input.sections.len() > 10
        || input.existing_pos.len() > 60
        || input.existing_pos.iter().any(|p| p.len() > 100)
        || input.examples.len() > 50
        || input
            .examples
            .iter()
            .any(|s| s.trim().is_empty() || s.chars().count() > 500)
        || input.senses.len() > 80
        || input.snapshot_id.len() != 64
        || !input.snapshot_id.bytes().all(|b| b.is_ascii_hexdigit())
        || input
            .senses
            .iter()
            .any(|s| s.pos.len() > 100 || s.gloss.chars().count() > 500)
    {
        return Err(language::invalid());
    }
    let parts = CatalogService::new(CatalogRepository::new(state.pool.clone()))
        .catalog()
        .await
        .map_err(AppError::internal)?;
    let catalog: BTreeMap<_, _> = parts
        .items
        .into_iter()
        .filter(|p| json!(p.kind) == kind)
        .map(|p| {
            let mut forms = vec![json!("base")];
            forms.extend(p.allowed_form_types.iter().map(|f| json!(f)));
            (p.code, forms)
        })
        .collect();
    if catalog.is_empty() {
        return Err(language::unavailable());
    }
    let matches = repository::list(
        &state.pool,
        NextListQuery {
            target_language: Some("en".into()),
            medium_locale: Some(medium.clone()),
            headword: Some(input.headword.clone()),
            kind: None,
            page: Some(1),
            page_size: Some(6),
        },
    )
    .await?;
    let mut materials: Vec<_> = matches
        .items
        .into_iter()
        .map(|entry| {
            let excerpt = format!(
                "{} [{}] {}",
                entry.headword,
                entry.part_of_speech.join(", "),
                entry.gloss.unwrap_or_default()
            );
            NextAgentSource {
                source: "智能词库".into(),
                locator: format!("{}@{}", entry.id, entry.revision),
                status: "found".into(),
                excerpt: excerpt.chars().take(1400).collect(),
            }
        })
        .collect();
    if materials.is_empty() {
        materials.push(NextAgentSource {
            source: "智能词库".into(),
            locator: String::new(),
            excerpt: String::new(),
            status: "no_match".into(),
        });
    }
    let scope = json!({
        "run_id": input.run_id, "actor_id": auth.subject, "entry_id": id,
        "base_revision": input.base_revision, "snapshot_id": input.snapshot_id,
        "headword": input.headword, "target": input.target, "medium": medium,
        "catalog": catalog, "senses": input.senses, "sections": input.sections,
        "existing_pos": input.existing_pos, "examples": input.examples,
    });
    let mut response = config
        .client
        .post(format!("{}/v2/lexicon/advance", config.base_url))
        .bearer_auth(&config.outbound_token)
        .header("Content-Type", "application/json")
        .body(json!({"scope":scope,"materials":materials}).to_string())
        .timeout(Duration::from_secs(50))
        .send()
        .await
        .map_err(|_| unavailable())?;
    if response.status() == reqwest::StatusCode::CONFLICT {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "生成范围已变化，请重新生成",
        ));
    }
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(AppError::rate_limited(
            ErrorCode::LexiconAssistantRateLimited,
            "生成批次过多，稍后再试",
        ));
    }
    if !response.status().is_success() {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > 300_000 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    let output: NextAgentResult = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    if output.run_id != input.run_id
        || output.base_revision != input.base_revision
        || output.snapshot_id != input.snapshot_id
        || !output.requires_human_review
    {
        return Err(unavailable());
    }
    editable(&state, &auth, id, input.base_revision).await?;
    Ok(Json(output))
}
