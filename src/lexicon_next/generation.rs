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
    state::AppState,
};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
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
    let mut materials = published_materials(&state.pool, &input.headword, &medium).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::{AdminRepository, AdminRole, NewAdmin};
    use sqlx::PgPool;

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
