//! Candidates are suggestions only; preview adoption uses the normal draft/save path.
use crate::{
    admin::{AdminAuth, permissions},
    api::ApiJson,
    catalog::{model::CatalogResponse, repository::CatalogRepository, service::CatalogService},
    error::{AppError, ErrorCode},
    lexicon::{
        dto::{AdminWordStatus, AdminWordV3, EntryKind, WordEntryKindV3},
        handler,
    },
    state::AppState,
};
use axum::{
    Json,
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconFormCandidatesInput {
    pub entry_id: Uuid,
    pub base_revision: i64,
    pub context_id: Uuid,
    pub catalog_version: i64,
    #[schema(min_length = 1, max_length = 100)]
    pub headword: String,
    #[schema(max_length = 24000)]
    pub local_content: String,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantCandidateVariant {
    #[schema(min_length = 1, max_length = 100)]
    pub spelling: String,
    #[schema(max_length = 200)]
    pub dict_phonetic: String,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantCandidateForm {
    pub form_type: String,
    pub is_regular: bool,
    pub uk: AssistantCandidateVariant,
    pub us: AssistantCandidateVariant,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantCandidatePos {
    pub pos: String,
    #[schema(min_items = 1, max_items = 10)]
    pub forms: Vec<AssistantCandidateForm>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelCandidates {
    pos: Vec<AssistantCandidatePos>,
    notes: Vec<String>,
    model: String,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconFormCandidatesResponse {
    pub entry_id: Uuid,
    pub base_revision: i64,
    pub context_id: Uuid,
    pub catalog_version: i64,
    pub headword: String,
    #[schema(min_items = 1, max_items = 8)]
    pub pos: Vec<AssistantCandidatePos>,
    pub notes: Vec<String>,
    pub usage: super::LexiconAssistantUsage,
    pub requires_human_review: bool,
}

async fn editable(
    state: &AppState,
    auth: &AdminAuth,
    input: &LexiconFormCandidatesInput,
) -> Result<AdminWordV3, AppError> {
    super::authorize(state, auth).await?;
    let word = handler::service(state)
        .get_v3(input.entry_id)
        .await
        .map_err(handler::map_error)?;
    permissions::reload(state, auth)
        .await?
        .require_owned_action("words.edit", word.created_by)?;
    if word.revision != input.base_revision {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "词条版本已更新，请重新生成候选",
        ));
    }
    if matches!(word.status, AdminWordStatus::Archived) {
        return Err(super::invalid("归档词条不能生成导入候选"));
    }
    Ok(word)
}

async fn catalog(state: &AppState, version: i64) -> Result<CatalogResponse, AppError> {
    let result = CatalogService::new(CatalogRepository::new(state.pool.clone()))
        .catalog()
        .await
        .map_err(|_| super::unavailable())?;
    if result.catalog_version != version {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "词性目录已更新，请刷新后重试",
        ));
    }
    Ok(result)
}

fn validate_output(
    output: &ModelCandidates,
    input: &LexiconFormCandidatesInput,
    catalog: &CatalogResponse,
    kind: WordEntryKindV3,
) -> Result<(), AppError> {
    let kind = match kind {
        WordEntryKindV3::Word => EntryKind::Word,
        WordEntryKindV3::Phrase => EntryKind::Phrase,
    };
    if output.pos.is_empty()
        || output.pos.len() > 8
        || output.notes.len() > 6
        || output.notes.iter().any(|note| note.chars().count() > 400)
        || output.model.is_empty()
        || output.model.len() > 100
    {
        return Err(super::unavailable());
    }
    let mut seen = std::collections::HashSet::new();
    for pos in &output.pos {
        if !seen.insert(&pos.pos) || pos.forms.is_empty() || pos.forms.len() > 10 {
            return Err(super::unavailable());
        }
        let item = catalog
            .items
            .iter()
            .find(|item| item.kind == kind && item.code == pos.pos)
            .ok_or_else(super::unavailable)?;
        let mut types = std::collections::HashSet::new();
        for form in &pos.forms {
            if !types.insert(&form.form_type)
                || (form.form_type != "base" && !item.allowed_form_types.contains(&form.form_type))
            {
                return Err(super::unavailable());
            }
            for variant in [&form.uk, &form.us] {
                if variant.spelling.trim().is_empty()
                    || variant.spelling != variant.spelling.trim()
                    || variant.spelling.chars().count() > 100
                    || variant.dict_phonetic.chars().count() > 200
                    || variant
                        .spelling
                        .chars()
                        .chain(variant.dict_phonetic.chars())
                        .any(char::is_control)
                {
                    return Err(super::unavailable());
                }
            }
        }
        let base = pos
            .forms
            .iter()
            .find(|form| form.form_type == "base")
            .ok_or_else(super::unavailable)?;
        let normalized = crate::lexicon::normalization::normalize_headword(&input.headword)
            .map_err(|_| super::unavailable())?
            .key;
        if ![&base.uk.spelling, &base.us.spelling]
            .iter()
            .any(|spelling| {
                crate::lexicon::normalization::normalize_headword(spelling)
                    .is_ok_and(|value| value.key == normalized)
            })
        {
            return Err(super::unavailable());
        }
    }
    Ok(())
}

#[utoipa::path(post,path="/api/v1/admin/lexicon/assistant/form-candidates",tag="admin-lexicon",security(("bearer_auth"=[])),request_body=LexiconFormCandidatesInput,
    responses((status=200,body=LexiconFormCandidatesResponse,description="AI 词性/词形/字典音标候选，仅供预览，不写入草稿"),
    (status=401,description="身份失效"),(status=403,description="缺少词条编辑权限"),(status=404,description="词条不存在"),(status=409,description="词条或目录版本变化"),(status=422,description="输入无效"),(status=429,description="请求预算受限"),(status=503,description="模型不可用或候选无效")))]
pub async fn generate(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<LexiconFormCandidatesInput>,
) -> Result<Response, AppError> {
    if input.base_revision < 1
        || input.headword.trim().is_empty()
        || input.headword.chars().count() > 100
        || input.headword.chars().any(char::is_control)
        || input.local_content.chars().count() > 24000
    {
        return Err(super::invalid("词条或当前编辑内容无效，请缩小范围后重试"));
    }
    let word = editable(&state, &auth, &input).await?;
    let catalog = catalog(&state, input.catalog_version).await?;
    let kind = match word.kind {
        WordEntryKindV3::Word => EntryKind::Word,
        WordEntryKindV3::Phrase => EntryKind::Phrase,
    };
    let allowed:Vec<_>=catalog.items.iter().filter(|item|item.kind==kind).map(|item|json!({"pos":item.code,"name":item.name_en,"form_types":std::iter::once("base".to_owned()).chain(item.allowed_form_types.clone()).collect::<Vec<_>>()})).collect();
    let lock = format!(
        "{}lexicon-assistant:pending:{}",
        state.login_throttle_prefix, auth.subject
    );
    let receipt = Uuid::now_v7().to_string();
    super::reserve(&state, &lock, &receipt).await?;
    let result=super::agent_request(&state,"form-candidates",Some(json!({"headword":input.headword,"local_content":input.local_content,"catalog":allowed}))).await;
    if let Ok(mut conn) = state.redis.get().await {
        let _=deadpool_redis::redis::Script::new("if redis.call('GET',KEYS[1])==ARGV[1] then return redis.call('DEL',KEYS[1]) end return 0").key(lock).arg(receipt).invoke_async::<i64>(&mut conn).await;
    }
    let output: ModelCandidates =
        serde_json::from_value(result?).map_err(|_| super::unavailable())?;
    editable(&state, &auth, &input).await?;
    let current_catalog = self::catalog(&state, input.catalog_version).await?;
    validate_output(&output, &input, &current_catalog, word.kind)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(LexiconFormCandidatesResponse {
            entry_id: input.entry_id,
            base_revision: input.base_revision,
            context_id: input.context_id,
            catalog_version: input.catalog_version,
            headword: input.headword,
            pos: output.pos,
            notes: output.notes,
            usage: super::LexiconAssistantUsage {
                model: output.model,
                model_calls: 1,
                tool_calls: 0,
                input_tokens: output.input_tokens,
                output_tokens: output.output_tokens,
            },
            requires_human_review: true,
        }),
    )
        .into_response())
}
