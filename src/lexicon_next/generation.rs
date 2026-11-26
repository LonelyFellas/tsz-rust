use super::{
    language,
    model::{NextDraft, strip_legacy_creation_detection},
};
use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiJson, ApiPath},
    catalog::{repository::CatalogRepository, service::CatalogService},
    error::{AppError, ErrorCode},
    lexicon::assistant::candidates::AssistantCandidatePos,
    question_bank::worker::AgentConfig,
    state::AppState,
};
use axum::{Json, extract::State, http::HeaderMap};
use chrono::{DateTime, Utc};
use deadpool_redis::redis::Script;
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
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
    /// Bypass cached candidates when generating forms or grammar independently.
    /// With the x-tsz-next-section-candidates: v1 header, also supports a single groups,
    /// senses, definitions, equivalents, relations, examples or translations section,
    /// or the ordered pair [senses, dialect].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force_refresh: bool,
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
    /// Recommended catalog code under this part of speech, when supported and known.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sub_pos: Option<String>,
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

#[derive(Clone, Copy, Serialize, Deserialize, ToSchema)]
pub enum NextDictionarySource {
    #[serde(rename = "有道")]
    Youdao,
    #[serde(rename = "Oxford Learner’s Dictionaries")]
    Oxford,
    #[serde(rename = "Merriam-Webster")]
    MerriamWebster,
    #[serde(rename = "Cambridge Dictionary")]
    Cambridge,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDictionaryRefreshInput {
    pub base_revision: i64,
    pub headword: String,
    pub source: NextDictionarySource,
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

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextGenerationCandidate {
    pub source_run_id: Uuid,
    pub generated_at: DateTime<Utc>,
    pub result: NextAgentResult,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextGenerationCandidates {
    #[schema(required = true)]
    pub candidate: Option<NextGenerationCandidate>,
}

#[derive(Default, Deserialize)]
pub struct GenerationCapability {
    pub generation: bool,
    #[serde(default)]
    pub generation_candidates: bool,
    #[serde(default)]
    pub section_generation_candidates: bool,
    #[serde(default)]
    pub grammar_sub_pos: bool,
    #[serde(default)]
    pub dictionary_refresh: bool,
}

pub async fn capability(state: &AppState) -> GenerationCapability {
    let Some(config) = &state.vocabulary_agent else {
        return GenerationCapability::default();
    };
    read_capability(config).await.unwrap_or_default()
}

async fn read_capability(config: &AgentConfig) -> Result<GenerationCapability, AppError> {
    let response = config
        .client
        .get(format!("{}/v2/lexicon/capability", config.base_url))
        .bearer_auth(&config.outbound_token)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| unavailable())?;
    decode(response).await
}

async fn editable(
    state: &AppState,
    auth: &AdminAuth,
    id: Uuid,
    revision: i64,
    sections: &[NextAgentSection],
) -> Result<(String, String), AppError> {
    let access = permissions::reload(state, auth).await?;
    access.require("words.access")?;
    if sections.iter().any(|section| {
        matches!(
            section,
            NextAgentSection::Examples | NextAgentSection::Translations
        )
    }) {
        access.require("sentences.access")?;
    }
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

async fn published_materials(
    pool: &sqlx::PgPool,
    headword: &str,
    medium: &str,
) -> Result<Vec<NextAgentSource>, AppError> {
    let rows: Vec<(Uuid, Uuid, Value)> = sqlx::query_as(
        "SELECT e.id, p.id, p.snapshot->'draft'
         FROM lexicon_next.entries e
         JOIN LATERAL (
             SELECT id, snapshot, published_at
             FROM lexicon_next.publications
             WHERE entry_id=e.id
             ORDER BY lifecycle_revision DESC
             LIMIT 1
         ) p ON true
         WHERE e.status='published' AND e.target_language='en' AND e.medium_locale=$2
           AND lower(p.snapshot #>> '{draft,headword}')=lower($1)
         ORDER BY p.published_at DESC, e.id
         LIMIT 6",
    )
    .bind(headword.trim())
    .bind(medium)
    .fetch_all(pool)
    .await
    .map_err(AppError::internal)?;
    rows.into_iter()
        .map(|(entry_id, publication_id, mut document)| {
            strip_legacy_creation_detection(&mut document);
            let draft: NextDraft = serde_json::from_value(document).map_err(AppError::internal)?;
            let positions = draft
                .senses
                .iter()
                .filter_map(|sense| sense.part_of_speech.as_deref())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let gloss = draft
                .senses
                .iter()
                .flat_map(|sense| &sense.definitions)
                .find(|row| row.locale == medium)
                .map(|row| row.text.as_str())
                .or_else(|| {
                    draft
                        .senses
                        .iter()
                        .flat_map(|sense| &sense.equivalents)
                        .find(|row| row.locale == medium)
                        .map(|row| row.text.as_str())
                })
                .unwrap_or_default();
            let excerpt = format!("{} [{}] {gloss}", draft.headword, positions.join(", "));
            Ok(NextAgentSource {
                source: "智能词库".into(),
                locator: format!("{entry_id}@{publication_id}"),
                status: "found".into(),
                excerpt: excerpt.chars().take(1400).collect(),
            })
        })
        .collect()
}

fn reusable_section(sections: &[NextAgentSection], extended: bool) -> Option<NextAgentSection> {
    use NextAgentSection::*;
    match sections {
        [section @ (Forms | Grammar)] => Some(*section),
        [
            section @ (Groups | Senses | Definitions | Equivalents | Relations | Examples
            | Translations),
        ] if extended => Some(*section),
        [Senses, Dialect] if extended => Some(Senses),
        _ => None,
    }
}

const SECTION_CANDIDATES_HEADER: &str = "x-tsz-next-section-candidates";
const SHARED_CANDIDATES_HEADER: &str = "x-tsz-next-shared-candidates";

pub(super) fn supports_section_candidates(headers: &HeaderMap) -> bool {
    headers
        .get(SECTION_CANDIDATES_HEADER)
        .is_some_and(|value| value == "v1")
}

#[derive(Serialize)]
struct GrammarSubPosOption {
    code: String,
    name_zh: String,
    name_en: String,
}

type GrammarSubPosCatalog = BTreeMap<String, Vec<GrammarSubPosOption>>;

fn supports_grammar_sub_pos(headers: &HeaderMap) -> bool {
    headers
        .get("x-tsz-next-grammar-sub-pos")
        .is_some_and(|value| value == "v1")
}

fn retain_grammar_sub_pos(rows: &mut [NextAgentGrammar], catalog: &GrammarSubPosCatalog) {
    for row in rows {
        if row.sub_pos.as_ref().is_some_and(|code| {
            !catalog
                .get(&row.pos)
                .is_some_and(|options| options.iter().any(|option| option.code == *code))
        }) {
            row.sub_pos = None;
        }
    }
}

async fn prepare<'a>(
    state: &'a AppState,
    auth: &AdminAuth,
    id: Uuid,
    input: &NextAgentInput,
    (kind, medium): (String, String),
    headers: &HeaderMap,
) -> Result<(&'a AgentConfig, String, GrammarSubPosCatalog), AppError> {
    language::text(&input.headword, 200)?;
    if input.run_id.is_nil()
        || (input.force_refresh
            && reusable_section(&input.sections, supports_section_candidates(headers)).is_none())
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
    let config = state.vocabulary_agent.as_deref().ok_or_else(unavailable)?;
    // A failed probe must not change the scope of an already admitted run.
    let recommend_sub_pos = supports_grammar_sub_pos(headers)
        && input.sections.contains(&NextAgentSection::Grammar)
        && read_capability(config).await?.grammar_sub_pos;
    let parts = CatalogService::new(CatalogRepository::new(state.pool.clone()))
        .catalog()
        .await
        .map_err(AppError::internal)?;
    let mut grammar_sub_pos = GrammarSubPosCatalog::new();
    let catalog: BTreeMap<_, _> = parts
        .items
        .into_iter()
        .filter(|p| json!(p.kind) == kind)
        .map(|p| {
            if recommend_sub_pos && !p.sub_parts.is_empty() {
                let options = p
                    .sub_parts
                    .into_iter()
                    .map(|sub_part| GrammarSubPosOption {
                        code: sub_part.code,
                        name_zh: sub_part.name_zh,
                        name_en: sub_part.full_name_en,
                    })
                    .collect();
                grammar_sub_pos.insert(p.code.clone(), options);
            }
            let mut forms = vec![json!("base")];
            forms.extend(p.allowed_form_types.iter().map(|f| json!(f)));
            (p.code, forms)
        })
        .collect();
    if catalog.is_empty() {
        return Err(language::unavailable());
    }
    let mut materials = published_materials(&state.pool, &input.headword, &medium).await?;
    if materials.is_empty() {
        materials.push(NextAgentSource {
            source: "智能词库".into(),
            locator: String::new(),
            excerpt: String::new(),
            status: "no_match".into(),
        });
    }
    let mut scope = json!({
        "run_id": input.run_id, "actor_id": auth.subject, "entry_id": id,
        "base_revision": input.base_revision, "snapshot_id": input.snapshot_id,
        "headword": input.headword, "target": input.target, "medium": medium,
        "catalog": catalog, "senses": input.senses, "sections": input.sections,
        "existing_pos": input.existing_pos, "examples": input.examples,
    });
    if !grammar_sub_pos.is_empty() {
        scope["grammar_sub_pos"] = json!(grammar_sub_pos);
    }
    Ok((
        config,
        json!({"scope":scope,"materials":materials}).to_string(),
        grammar_sub_pos,
    ))
}

async fn send(
    config: &AgentConfig,
    operation: &str,
    body: &str,
    timeout: Duration,
    shared_candidates: bool,
) -> Result<Response, AppError> {
    let request = config
        .client
        .post(format!("{}/v2/lexicon/{operation}", config.base_url))
        .bearer_auth(&config.outbound_token)
        .header("Content-Type", "application/json")
        .body(body.to_owned())
        .timeout(timeout);
    let request = if shared_candidates {
        request
            .header(SECTION_CANDIDATES_HEADER, "v1")
            .header(SHARED_CANDIDATES_HEADER, "v1")
    } else {
        request
    };
    request.send().await.map_err(|_| unavailable())
}

async fn decode<T: DeserializeOwned>(mut response: Response) -> Result<T, AppError> {
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
    serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

fn validate_binding(output: &NextAgentResult, input: &NextAgentInput) -> Result<(), AppError> {
    if output.run_id != input.run_id
        || output.base_revision != input.base_revision
        || output.snapshot_id != input.snapshot_id
        || !output.requires_human_review
    {
        return Err(unavailable());
    }
    Ok(())
}

fn endpoint_missing(response: &Response) -> bool {
    matches!(
        response.status(),
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
    )
}

#[utoipa::path(post,path="/api/v1/admin/lexicon-next/entries/{id}/generation",tag="admin-lexicon-next",security(("bearer_auth"=[])),
    params(("id"=Uuid,Path),("x-tsz-next-grammar-sub-pos"=Option<String>,Header,description="v1 accepts catalog-scoped grammar subcategory recommendations."),("x-tsz-next-section-candidates"=Option<String>,Header,description="v1 enables reuse for independent section generation.")),request_body=NextAgentInput,responses((status=200,body=NextAgentResult),(status=401),(status=403),(status=404),(status=409),(status=422),(status=429),(status=503)))]
pub async fn advance(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<NextAgentInput>,
) -> Result<Json<NextAgentResult>, AppError> {
    let entry = editable(&state, &auth, id, input.base_revision, &input.sections).await?;
    let (config, body, grammar_sub_pos) =
        prepare(&state, &auth, id, &input, entry, &headers).await?;
    let shared_candidates =
        supports_section_candidates(&headers) && reusable_section(&input.sections, true).is_some();
    let timeout = Duration::from_secs(50);
    let response = if input.force_refresh {
        let refresh = send(config, "refresh", &body, timeout, shared_candidates).await?;
        if endpoint_missing(&refresh) {
            send(config, "advance", &body, timeout, shared_candidates).await?
        } else {
            refresh
        }
    } else {
        send(config, "advance", &body, timeout, shared_candidates).await?
    };
    if response.status() == StatusCode::CONFLICT {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "生成范围已变化，请重新生成",
        ));
    }
    if response.status() == StatusCode::TOO_MANY_REQUESTS {
        return Err(AppError::rate_limited(
            ErrorCode::LexiconAssistantRateLimited,
            "生成批次过多，稍后再试",
        ));
    }
    let mut output = decode(response).await?;
    validate_binding(&output, &input)?;
    retain_grammar_sub_pos(&mut output.proposals.grammar, &grammar_sub_pos);
    editable(&state, &auth, id, input.base_revision, &input.sections).await?;
    Ok(Json(output))
}

async fn reserve_dictionary_refresh(state: &AppState, actor: Uuid) -> Result<(), AppError> {
    let key = format!(
        "{}lexicon-next:dictionary-refresh:{actor}",
        state.login_throttle_prefix
    );
    let mut conn = state.redis.get().await.map_err(|_| unavailable())?;
    let retry_after: u64 = Script::new(
        r#"
        if tonumber(redis.call('GET', KEYS[1]) or '0') >= tonumber(ARGV[1]) then
            return math.max(redis.call('TTL', KEYS[1]), 1)
        end
        if redis.call('INCR', KEYS[1]) == 1 then
            redis.call('EXPIRE', KEYS[1], ARGV[2])
        end
        return 0
        "#,
    )
    .key(key)
    .arg(12)
    .arg(600)
    .invoke_async(&mut conn)
    .await
    .map_err(|_| unavailable())?;
    if retry_after > 0 {
        return Err(AppError::rate_limited(
            ErrorCode::LexiconAssistantRateLimited,
            "词典刷新过于频繁，请稍后重试",
        )
        .with_retry_after(retry_after));
    }
    Ok(())
}

#[utoipa::path(post,path="/api/v1/admin/lexicon-next/entries/{id}/generation/dictionary-reference",tag="admin-lexicon-next",security(("bearer_auth"=[])),
    params(("id"=Uuid,Path)),request_body=NextDictionaryRefreshInput,responses((status=200,body=NextAgentSource),(status=401),(status=403),(status=404),(status=409),(status=422),(status=429),(status=503)))]
pub async fn dictionary_reference(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<NextDictionaryRefreshInput>,
) -> Result<Json<NextAgentSource>, AppError> {
    language::text(&input.headword, 200)?;
    let headword = input.headword.trim();
    editable(&state, &auth, id, input.base_revision, &[]).await?;
    let config = state.vocabulary_agent.as_deref().ok_or_else(unavailable)?;
    reserve_dictionary_refresh(&state, auth.subject).await?;
    let source = json!(input.source);
    let response = send(
        config,
        "dictionary-reference",
        &json!({ "headword": headword, "source": source }).to_string(),
        Duration::from_secs(15),
        false,
    )
    .await?;
    let output: NextAgentSource = decode(response).await?;
    if source.as_str() != Some(output.source.as_str())
        || !matches!(
            output.status.as_str(),
            "found" | "no_match" | "unavailable" | "not_configured"
        )
    {
        return Err(unavailable());
    }
    editable(&state, &auth, id, input.base_revision, &[]).await?;
    Ok(Json(output))
}

#[utoipa::path(post,path="/api/v1/admin/lexicon-next/entries/{id}/generation/candidates",tag="admin-lexicon-next",security(("bearer_auth"=[])),
    params(("id"=Uuid,Path),("x-tsz-next-grammar-sub-pos"=Option<String>,Header,description="v1 accepts catalog-scoped grammar subcategory recommendations."),("x-tsz-next-section-candidates"=Option<String>,Header,description="v1 enables lookup for independent section generation.")),request_body=NextAgentInput,responses((status=200,body=NextGenerationCandidates),(status=401),(status=403),(status=404),(status=409),(status=422),(status=503)))]
pub async fn candidates(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<NextAgentInput>,
) -> Result<Json<NextGenerationCandidates>, AppError> {
    let entry = editable(&state, &auth, id, input.base_revision, &input.sections).await?;
    let shared_candidates = supports_section_candidates(&headers);
    reusable_section(&input.sections, shared_candidates)
        .filter(|_| !input.force_refresh)
        .ok_or_else(language::invalid)?;
    let (config, body, grammar_sub_pos) =
        prepare(&state, &auth, id, &input, entry, &headers).await?;
    let response = send(
        config,
        "candidates",
        &body,
        Duration::from_secs(8),
        shared_candidates,
    )
    .await?;
    let mut output: NextGenerationCandidates = if endpoint_missing(&response) {
        NextGenerationCandidates { candidate: None }
    } else {
        decode(response).await?
    };
    if let Some(candidate) = &mut output.candidate {
        let result = &mut candidate.result;
        validate_binding(result, &input)?;
        let populated = input.sections.iter().any(|section| match section {
            NextAgentSection::Forms => !result.proposals.forms.is_empty(),
            NextAgentSection::Grammar => !result.proposals.grammar.is_empty(),
            NextAgentSection::Groups => !result.proposals.groups.is_empty(),
            NextAgentSection::Senses => !result.proposals.senses.is_empty(),
            NextAgentSection::Dialect => !result.proposals.dialect.is_empty(),
            NextAgentSection::Definitions => !result.proposals.definitions.is_empty(),
            NextAgentSection::Equivalents => !result.proposals.equivalents.is_empty(),
            NextAgentSection::Relations => !result.proposals.relations.is_empty(),
            NextAgentSection::Examples => !result.proposals.examples.is_empty(),
            NextAgentSection::Translations => !result.proposals.translations.is_empty(),
        });
        if !result.done
            || !populated
            || !result.steps.iter().all(|step| step.state == "completed")
            || !result
                .steps
                .iter()
                .map(|step| step.section)
                .eq(input.sections.iter().copied())
        {
            return Err(unavailable());
        }
        retain_grammar_sub_pos(&mut result.proposals.grammar, &grammar_sub_pos);
    }
    editable(&state, &auth, id, input.base_revision, &input.sections).await?;
    Ok(Json(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::{AdminRepository, AdminRole, NewAdmin};
    use sqlx::PgPool;

    #[test]
    fn new_reusable_sections_require_client_negotiation() {
        use NextAgentSection::*;

        assert_eq!(reusable_section(&[Forms], false), Some(Forms));
        assert_eq!(reusable_section(&[Grammar], false), Some(Grammar));
        for sections in [&[Senses][..], &[Senses, Dialect]] {
            assert_eq!(reusable_section(sections, false), None);
            assert_eq!(reusable_section(sections, true), Some(Senses));
        }
        assert_eq!(reusable_section(&[Dialect], true), None);
    }

    #[tokio::test]
    async fn capability_failure_is_distinct_from_a_legacy_response() {
        use axum::{Router, response::IntoResponse, routing::get};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let failed_once = Arc::new(AtomicBool::new(false));
        let router = Router::new().route(
            "/v2/lexicon/capability",
            get(move || {
                let failed_once = failed_once.clone();
                async move {
                    if !failed_once.swap(true, Ordering::Relaxed) {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    Json(json!({"generation": true, "generation_candidates": true})).into_response()
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let config = AgentConfig {
            base_url: format!("http://{address}"),
            outbound_token: "isolated-probe".into(),
            client: reqwest::Client::new(),
        };

        assert!(read_capability(&config).await.is_err());
        let legacy = read_capability(&config).await.unwrap();
        assert!(legacy.generation && legacy.generation_candidates);
        assert!(!legacy.grammar_sub_pos && !legacy.dictionary_refresh);
        server.abort();
    }

    #[test]
    fn grammar_recommendations_only_surface_for_current_catalog_options() {
        let mut rows: Vec<NextAgentGrammar> = serde_json::from_value(json!([
            {"pos": "verb", "pattern": "rely on someone", "sub_pos": "V-I"},
            {"pos": "noun", "pattern": "a reliance", "sub_pos": "V-I"},
            {"pos": "verb", "pattern": "rely upon someone"}
        ]))
        .unwrap();
        let mut headers = HeaderMap::new();
        assert!(!supports_grammar_sub_pos(&headers));
        headers.insert("x-tsz-next-grammar-sub-pos", "v1".parse().unwrap());
        assert!(supports_grammar_sub_pos(&headers));
        let catalog = GrammarSubPosCatalog::from([(
            "verb".into(),
            vec![GrammarSubPosOption {
                code: "V-I".into(),
                name_zh: "不及物动词".into(),
                name_en: "intransitive verb".into(),
            }],
        )]);
        retain_grammar_sub_pos(&mut rows, &catalog);
        assert_eq!(rows[0].sub_pos.as_deref(), Some("V-I"));
        assert!(rows[1].sub_pos.is_none());
        assert!(rows[2].sub_pos.is_none());
        retain_grammar_sub_pos(&mut rows, &GrammarSubPosCatalog::new());
        assert!(
            serde_json::to_value(&rows[0])
                .unwrap()
                .get("sub_pos")
                .is_none()
        );
    }

    #[sqlx::test]
    async fn reference_materials_use_only_the_effective_published_snapshot(pool: PgPool) {
        let actor = Uuid::new_v4();
        AdminRepository::new(pool.clone())
            .create(NewAdmin {
                id: actor,
                phone: "generation-test".into(),
                display_name: "本地测试".into(),
                password_hash: "test-only".into(),
                role: AdminRole::Admin,
                must_change_password: false,
                created_by_admin_id: None,
            })
            .await
            .unwrap();

        let publication_id = Uuid::new_v4();
        let mut expected_entry = Uuid::nil();
        for status in ["published", "draft", "archived"] {
            let entry = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO lexicon_next.entries
                 (id,target_language,medium_locale,kind,headword,owner_id,revision,status,lifecycle_revision)
                 VALUES($1,'en','zh-Hans','word','private revised spelling',$2,2,$3,1)",
            )
            .bind(entry)
            .bind(actor)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
            let draft = |text: &str| {
                json!({
                    "schema_version": 1, "headword": "center", "forms": [],
                    "senses": [{"id": Uuid::new_v4(), "part_of_speech":"noun",
                        "definitions":[{"id":Uuid::new_v4(),"locale":"zh-Hans","style":"definition","text":text}],
                        "equivalents":[]}]
                })
            };
            let published = draft("已发布的词义");
            sqlx::query(
                "INSERT INTO lexicon_next.draft_revisions(entry_id,revision,actor_id,document)
                 VALUES($1,1,$2,$3),($1,2,$2,$4)",
            )
            .bind(entry)
            .bind(actor)
            .bind(&published)
            .bind(draft("未发布的草稿内容"))
            .execute(&pool)
            .await
            .unwrap();
            if status != "draft" {
                let record = if status == "published" {
                    expected_entry = entry;
                    publication_id
                } else {
                    Uuid::new_v4()
                };
                sqlx::query(
                    "INSERT INTO lexicon_next.publications
                     (id,entry_id,revision,lifecycle_revision,published_by_admin_id,snapshot)
                     VALUES($1,$2,1,1,$3,$4)",
                )
                .bind(record)
                .bind(entry)
                .bind(actor)
                .bind(json!({"draft":published,"sentence_resources":[],"relation_targets":[]}))
                .execute(&pool)
                .await
                .unwrap();
            }
        }

        let result = published_materials(&pool, "Center", "zh-Hans")
            .await
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].locator,
            format!("{expected_entry}@{publication_id}")
        );
        assert_eq!(result[0].excerpt, "center [noun] 已发布的词义");
        assert!(
            published_materials(&pool, "private revised spelling", "zh-Hans")
                .await
                .unwrap()
                .is_empty()
        );
    }
}
