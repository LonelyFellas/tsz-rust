//! Single-entry publication. See tsz/docs/features/lexicon-next-publishing/README.md.
use super::{language, model::*, publication_model::*};
use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiJson, ApiPath},
    error::{AppError, ErrorCode},
    state::AppState,
};
use axum::{Json, extract::State};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn conflict(message: &str) -> AppError {
    AppError::conflict(ErrorCode::RevisionConflict, None, message)
}
fn issue(id: Uuid, field: &str, message: &str) -> NextPublicationIssue {
    NextPublicationIssue {
        node_id: id,
        field: field.into(),
        message: message.into(),
    }
}
const SELECT_PUBLICATION: &str = "SELECT id,entry_id,revision,lifecycle_revision,published_by_admin_id,published_at,snapshot FROM lexicon_next.publications WHERE entry_id=$1 ORDER BY lifecycle_revision DESC LIMIT 1";
fn decode(
    row: sqlx::postgres::PgRow,
    headers: &axum::http::HeaderMap,
) -> Result<NextPublication, AppError> {
    let mut snapshot: serde_json::Value = row.get("snapshot");
    if let Some(draft) = snapshot.get_mut("draft") {
        super::model::strip_legacy_creation_detection(draft);
        if !super::supports_form_questions(headers)
            && let Some(fields) = draft.as_object_mut()
        {
            fields.remove("form_question_eligibility");
        }
    }
    Ok(NextPublication {
        id: row.get("id"),
        entry_id: row.get("entry_id"),
        revision: row.get("revision"),
        lifecycle_revision: row.get("lifecycle_revision"),
        published_by_admin_id: row.get("published_by_admin_id"),
        published_at: row.get("published_at"),
        snapshot: serde_json::from_value(snapshot).map_err(AppError::internal)?,
    })
}

#[utoipa::path(get, path="/api/v1/admin/lexicon-next/entries/{id}/publication", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("x-tsz-next-form-questions"=Option<String>,Header,description="v1 exposes concrete form question eligibility"),("id"=Uuid, Path),("x-tsz-next-dialect-pairs"=Option<String>, Header, description="v1: only reciprocal currently published peers are navigable")), responses((status=200, body=NextPublicationState), (status=401), (status=403), (status=404)))]
pub async fn read(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: axum::http::HeaderMap,
) -> Result<Json<NextPublicationState>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let row: (String, i64, i64) = sqlx::query_as(
        "SELECT status,revision,lifecycle_revision FROM lexicon_next.entries WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("entry not found"))?;
    let current = sqlx::query(SELECT_PUBLICATION)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AppError::internal)?
        .map(|row| decode(row, &headers))
        .transpose()?;
    let mut changed = current.as_ref().is_none_or(|p| p.revision != row.1);
    if let Some(publication) = &current {
        for sentence in &publication.snapshot.sentence_resources {
            let revision: Option<i64> =
                sqlx::query_scalar("SELECT revision FROM lexicon_next.sentences WHERE id=$1")
                    .bind(sentence.id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(AppError::internal)?;
            changed |= revision != Some(sentence.revision);
        }
    }
    let dialect_senses = if super::supports_pairs(&headers) && row.0 == "published" {
        match &current {
            Some(p) => Some(super::dialect_pairs::project(&mut tx, id, p.revision, true).await?),
            None => None,
        }
    } else {
        None
    };
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(NextPublicationState {
        dialect_senses,
        entry_id: id,
        status: match row.0.as_str() {
            "published" => super::list_model::NextWorkspaceStatus::Published,
            "archived" => super::list_model::NextWorkspaceStatus::Archived,
            _ => super::list_model::NextWorkspaceStatus::Draft,
        },
        lifecycle_revision: row.2,
        has_unpublished_changes: changed,
        current,
    }))
}

struct Prepared {
    preview: NextPublicationPreview,
    snapshot: NextPublicationSnapshot,
    assets: BTreeMap<Uuid, Uuid>,
    taxonomies: BTreeSet<Uuid>,
}

async fn prepare(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    auth: &AdminAuth,
    id: Uuid,
    base: &NextPublicationBase,
) -> Result<Prepared, AppError> {
    if base.base_revision < 1 || base.base_lifecycle_revision < 1 {
        return Err(language::invalid());
    }
    permissions::lock_session(tx, auth, "words.publish").await?;
    let access = permissions::lock(tx, auth.subject).await?;
    super::repository::lock_list_writers(tx).await?;
    let (owner, revision, lifecycle, status): (Uuid, i64, i64, String) = sqlx::query_as("SELECT owner_id,revision,lifecycle_revision,status FROM lexicon_next.entries WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?.ok_or_else(|| AppError::not_found("entry not found"))?;
    access.require_owned_action("words.publish", owner)?;
    if status == "archived" {
        return Err(AppError::conflict(
            ErrorCode::EntryArchived,
            None,
            "归档词条不能发布，请先恢复",
        ));
    }
    if revision != base.base_revision || lifecycle != base.base_lifecycle_revision {
        return Err(conflict("词条版本已变化，请重新加载并检查发布"));
    }
    // Writers hold the list lock, so the document cannot change between this read and commit.
    let entry = super::repository::get_on(tx, id).await?;
    language::validate(&entry.target_language, &entry.draft)?;
    let mut checked = entry.draft.clone();
    super::equivalents::prepare(tx, &mut checked, &entry.draft, true).await?;
    let parents = sqlx::query_as::<_, (String, String)>("SELECT s.code,p.code::text FROM catalog.sub_parts_of_speech s JOIN catalog.parts_of_speech p ON p.id=s.part_of_speech_id ORDER BY p.id,s.id FOR SHARE OF p,s")
        .fetch_all(&mut **tx).await.map_err(AppError::internal)?
        .into_iter().collect();
    let mut issues = completeness(
        id,
        &entry.draft,
        entry.medium_locale.as_deref().unwrap_or("zh-Hans"),
    );
    if let Some(editor) = &entry.draft.form_editor {
        let resolved = editor
            .clone()
            .resolve()
            .map_err(|error| error.into_app_error())?;
        issues.extend(
            crate::lexicon::v3_contract::validate_forms(
                &resolved,
                crate::lexicon::dto::StepSaveIntent::Complete,
            )
            .into_iter()
            .map(|v| {
                let label = match v.field.as_str() {
                    "pos" => "基本词性",
                    "form_groups" => "词形变化组",
                    "members" => "变化组成员",
                    "spelling" => "词形拼写",
                    "pronunciations" => "读音",
                    "dict_phonetic" => "字典音标",
                    "actual_pron" => "实际发音",
                    _ => "词形内容",
                };
                issue(v.node_id, &v.field, &format!("请补齐{label}"))
            }),
        );
    }
    for sense in &entry.draft.senses {
        if !super::grammar::valid_subcategory(
            sense.part_of_speech.as_deref().unwrap_or(""),
            sense.sub_pos.as_deref().unwrap_or(""),
            &parents,
        ) {
            issues.push(issue(sense.id, "sub_pos", "请选择词义的有效细分词性"));
        }
    }
    issues.extend(
        super::grammar::issues(&entry.draft, &parents)
            .into_iter()
            .map(|v| issue(v.node_id, &v.field, &v.message)),
    );
    let sentence_ids: Vec<_> = entry
        .draft
        .senses
        .iter()
        .flat_map(|s| s.sentence_ids.iter().flatten().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if !sentence_ids.is_empty() {
        access.require("sentences.access")?;
    }
    // Same order as sentence saves; the frozen content remains stable through commit.
    sqlx::query("SELECT id FROM lexicon_next.sentences WHERE id=ANY($1) ORDER BY id FOR SHARE")
        .bind(&sentence_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    let sentence_resources = super::sentence_drafts::resources(tx, &entry.draft).await?;
    let mut warnings = Vec::new();
    for sentence in &sentence_resources {
        access.require_owned_action("sentences.publish", sentence.owner_id)?;
        if sentence.target_language != entry.target_language || sentence.draft.variants.is_empty() {
            issues.push(issue(
                sentence.id,
                "sentence_ids",
                "关联例句的目标语言或内容无效",
            ));
        }
        for variant in &sentence.draft.variants {
            if variant.content.text().trim().is_empty() {
                issues.push(issue(variant.id, "content", "请填写例句原文"));
            }
        }
    }
    let pairs = super::dialect_pairs::project(tx, id, revision, true).await?;
    for sense in &pairs {
        if sense.paired && sense.target.is_none() {
            warnings.push(issue(
                sense.sense_id,
                "dialect_pairs",
                "对方尚未发布同一配对，本侧可先发布；双方发布后链接生效",
            ));
        }
    }
    let mut relation_targets = Vec::new();
    for sense in &entry.draft.senses {
        for relation in sense.relations.iter().flatten() {
            if let (Some(target), Some(target_sense)) =
                (relation.target_word_id, relation.target_sense_id)
            {
                let target_publication = sqlx::query("SELECT p.id,p.snapshot FROM lexicon_next.entries e JOIN LATERAL (SELECT id,snapshot FROM lexicon_next.publications WHERE entry_id=e.id ORDER BY lifecycle_revision DESC LIMIT 1) p ON true WHERE e.id=$1 AND e.status='published' AND e.target_language=$2 AND e.medium_locale=$3").bind(target).bind(&entry.target_language).bind(&entry.medium_locale).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
                if let Some(row) = target_publication {
                    let mut snapshot_value: serde_json::Value = row.get("snapshot");
                    if let Some(draft) = snapshot_value.get_mut("draft") {
                        super::model::strip_legacy_creation_detection(draft);
                    }
                    let snapshot: NextPublicationSnapshot =
                        serde_json::from_value(snapshot_value).map_err(AppError::internal)?;
                    if snapshot.draft.senses.iter().any(|s| s.id == target_sense) {
                        let sense = snapshot
                            .draft
                            .senses
                            .iter()
                            .find(|s| s.id == target_sense)
                            .expect("checked above");
                        let medium = entry.medium_locale.as_deref().unwrap_or("zh-Hans");
                        let gloss = sense
                            .definitions
                            .iter()
                            .find(|d| d.locale == medium)
                            .map(|d| d.text.clone())
                            .or_else(|| {
                                sense
                                    .equivalents
                                    .iter()
                                    .find(|e| e.locale == medium)
                                    .map(|e| e.text.clone())
                            })
                            .unwrap_or_default();
                        relation_targets.push(NextPublicationRelation {
                            entry_id: target,
                            sense_id: target_sense,
                            publication_id: row.get("id"),
                            headword: snapshot.draft.headword.clone(),
                            forms: super::forms::projection(&snapshot.draft)?,
                            gloss,
                        });
                        continue;
                    }
                }
                issues.push(issue(
                    relation.id,
                    "relations",
                    "关联拓展词或目标词义尚未发布，请先发布目标或改为待关联文本",
                ));
            } else {
                warnings.push(issue(
                    relation.id,
                    "relations",
                    "拓展词尚未关联，将仅显示已填写的文本",
                ));
            }
        }
    }
    let snapshot = NextPublicationSnapshot {
        draft: entry.draft,
        sentence_resources,
        relation_targets,
    };
    let mut assets = BTreeMap::new();
    let document = serde_json::to_value(&snapshot.draft).map_err(AppError::internal)?;
    collect_assets(&document, id, &mut assets)?;
    for sentence in &snapshot.sentence_resources {
        collect_assets(
            &serde_json::to_value(&sentence.draft).map_err(AppError::internal)?,
            sentence.id,
            &mut assets,
        )?;
    }
    let ids: Vec<_> = assets.keys().copied().collect();
    sqlx::query("SELECT id FROM lexicon.audio_assets WHERE id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(&ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    for (asset, resource) in &assets {
        let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon.audio_assets a WHERE a.id=$1 AND a.reclamation_started_at IS NULL AND NOT EXISTS(SELECT 1 FROM lexicon.audio_generations g WHERE g.asset_id=a.id AND g.state<>'ready') AND (EXISTS(SELECT 1 FROM lexicon_next.audio_asset_references r WHERE r.asset_id=a.id AND r.entry_id=$2) OR EXISTS(SELECT 1 FROM lexicon_next.sentence_audio_asset_references r WHERE r.asset_id=a.id AND r.sentence_id=$2)))").bind(asset).bind(resource).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
        if !valid {
            issues.push(issue(
                *resource,
                "audio_assets",
                "音频资产未就绪、已失效或归属不匹配",
            ));
        }
    }
    let mut taxonomies = BTreeSet::new();
    collect_taxonomies(
        &serde_json::to_value(&snapshot).map_err(AppError::internal)?,
        &mut taxonomies,
    )?;
    let taxonomy_ids: Vec<_> = taxonomies.iter().copied().collect();
    let existing = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM catalog.lexicon_taxonomies WHERE id=ANY($1) ORDER BY id FOR KEY SHARE",
    )
    .bind(&taxonomy_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    if existing.len() != taxonomy_ids.len() {
        issues.push(issue(
            id,
            "metadata",
            "语体或行业配置已失效，请重新选择后发布",
        ));
    }
    // Text publication is allowed; it never grants listening or assessment eligibility.
    audio_warnings(&document, &mut warnings);
    for sentence in &snapshot.sentence_resources {
        audio_warnings(
            &serde_json::to_value(&sentence.draft).map_err(AppError::internal)?,
            &mut warnings,
        );
    }
    // Catalog iteration must be stable between preview and publish.
    let catalog: BTreeMap<_, _> = parents.into_iter().collect();
    let token = Sha256::digest(
        serde_json::to_vec(&(
            id,
            &entry.target_language,
            &entry.medium_locale,
            &snapshot,
            &pairs,
            catalog,
            revision,
            lifecycle,
        ))
        .map_err(AppError::internal)?,
    )
    .iter()
    .map(|v| format!("{v:02x}"))
    .collect::<String>();
    Ok(Prepared {
        preview: NextPublicationPreview {
            entry_id: id,
            revision,
            lifecycle_revision: lifecycle,
            preview_token: token,
            can_publish: issues.is_empty(),
            issues,
            warnings,
        },
        snapshot,
        assets,
        taxonomies,
    })
}

fn collect_taxonomies(value: &Value, ids: &mut BTreeSet<Uuid>) -> Result<(), AppError> {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                if matches!(key.as_str(), "register_ids" | "domain_ids") {
                    for id in child.as_array().ok_or_else(language::invalid)? {
                        ids.insert(
                            serde_json::from_value(id.clone()).map_err(|_| language::invalid())?,
                        );
                    }
                } else if key == "domain_id" && !child.is_null() {
                    ids.insert(
                        serde_json::from_value(child.clone()).map_err(|_| language::invalid())?,
                    );
                }
                collect_taxonomies(child, ids)?;
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_taxonomies(child, ids)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn collect_assets(
    value: &Value,
    resource: Uuid,
    ids: &mut BTreeMap<Uuid, Uuid>,
) -> Result<(), AppError> {
    for (_, assets) in crate::speech::generated_references::asset_references(value) {
        for asset in assets {
            ids.insert(asset, resource);
        }
    }
    fn recordings(
        value: &Value,
        resource: Uuid,
        ids: &mut BTreeMap<Uuid, Uuid>,
    ) -> Result<(), AppError> {
        if let Some(rows) = value.get("audio_assets").and_then(Value::as_array) {
            for row in rows {
                let asset = row
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|v| Uuid::parse_str(v).ok())
                    .ok_or_else(language::invalid)?;
                ids.insert(asset, resource);
            }
        }
        match value {
            Value::Object(fields) => {
                for child in fields.values() {
                    recordings(child, resource, ids)?;
                }
            }
            Value::Array(items) => {
                for child in items {
                    recordings(child, resource, ids)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    recordings(value, resource, ids)
}
fn audio_warnings(value: &Value, warnings: &mut Vec<NextPublicationIssue>) {
    if let Some(id) = value
        .get("id")
        .and_then(Value::as_str)
        .and_then(|v| Uuid::parse_str(v).ok())
    {
        let has_text = value
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
            || value
                .get("content")
                .and_then(|v| v.get("text"))
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty());
        let english = value
            .get("locale")
            .and_then(Value::as_str)
            .is_some_and(|s| s == "en" || s.starts_with("en-"))
            || value.get("actual_pron").is_some();
        let mut assets = BTreeMap::new();
        let _ = collect_assets(value, id, &mut assets);
        if (has_text || value.get("actual_pron").is_some()) && english && assets.is_empty() {
            warnings.push(issue(
                id,
                "audio_assets",
                "部分内容缺少音频，将按文本发布。",
            ));
        }
    }
    match value {
        Value::Object(fields) => {
            for child in fields.values() {
                audio_warnings(child, warnings);
            }
        }
        Value::Array(items) => {
            for child in items {
                audio_warnings(child, warnings);
            }
        }
        _ => {}
    }
}
fn completeness(id: Uuid, draft: &NextDraft, medium: &str) -> Vec<NextPublicationIssue> {
    let mut issues = Vec::new();
    let forms = draft.form_editor.as_ref();
    if forms.is_none_or(|f| f.pos.is_empty()) {
        issues.push(issue(id, "form_editor", "请至少添加一种基本词性和原形"));
    }
    if let Some(editor) = forms {
        for pos in &editor.pos {
            if !pos.forms.iter().any(|f| f.form_type == "base") {
                issues.push(issue(pos.pos_id, "forms", "每种基本词性至少需要一个原形"));
            }
        }
        let projected = super::forms::projection(draft).unwrap_or_default();
        for locale in ["en-GB", "en-US"] {
            if !projected
                .iter()
                .any(|f| (f.locale == locale || f.locale == "en") && !f.text.trim().is_empty())
            {
                issues.push(issue(id, "forms", &format!("请填写{locale}词形")));
            }
        }
    }
    if draft.senses.is_empty() {
        issues.push(issue(id, "senses", "请至少添加一个词义"));
    }
    let groups = draft.sense_groups.as_deref().unwrap_or_default();
    for group in groups {
        if group.name_zh.trim().is_empty() || group.name_en.trim().is_empty() {
            issues.push(issue(
                group.id,
                "sense_groups",
                "请填写语义区间的中英文名称",
            ));
        }
    }
    for sense in &draft.senses {
        if sense
            .part_of_speech
            .as_ref()
            .is_none_or(|pos| forms.is_none_or(|f| !f.pos.iter().any(|p| &p.pos == pos)))
        {
            issues.push(issue(
                sense.id,
                "part_of_speech",
                "请选择已添加原形的基本词性",
            ));
        }
        if sense.level.is_none() {
            issues.push(issue(sense.id, "level", "请选择词义等级"));
        }
        if sense
            .sense_group_id
            .is_none_or(|id| !groups.iter().any(|g| g.id == id))
        {
            issues.push(issue(sense.id, "sense_group_id", "请选择语义区间"));
        }
        for locale in ["en-GB", "en-US"] {
            if !sense
                .definitions
                .iter()
                .any(|d| (d.locale == locale || d.locale == "en") && !d.text.trim().is_empty())
            {
                issues.push(issue(
                    sense.id,
                    "definitions",
                    &format!("请填写{locale}目标语释义"),
                ));
            }
        }
        if !sense
            .definitions
            .iter()
            .any(|d| d.locale == medium && !d.text.trim().is_empty())
            && !sense
                .equivalents
                .iter()
                .any(|e| e.locale == medium && !e.text.trim().is_empty())
            && !sense.no_equivalents.iter().any(|v| v == medium)
        {
            issues.push(issue(
                sense.id,
                "equivalents",
                "请填写所属媒介语言的解释或译词，或明确无对应译词",
            ));
        }
        for definition in &sense.definitions {
            if definition.level.is_none() {
                issues.push(issue(definition.id, "level", "请选择释义等级"));
            }
        }
    }
    issues
}

#[utoipa::path(post, path="/api/v1/admin/lexicon-next/entries/{id}/publication-preview", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("id"=Uuid, Path)), request_body=NextPublicationBase, responses((status=200, body=NextPublicationPreview), (status=401), (status=403), (status=404), (status=409), (status=422)))]
pub async fn preview(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(base): ApiJson<NextPublicationBase>,
) -> Result<Json<NextPublicationPreview>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let prepared = prepare(&mut tx, &auth, id, &base).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(prepared.preview))
}

#[utoipa::path(post, path="/api/v1/admin/lexicon-next/entries/{id}/publications", tag="admin-lexicon-next", security(("bearer_auth"=[])), params(("x-tsz-next-form-questions"=Option<String>,Header,description="v1 exposes concrete form question eligibility"),("id"=Uuid, Path)), request_body=NextPublishInput, responses((status=200, body=NextPublication), (status=401), (status=403), (status=404), (status=409), (status=422)))]
pub async fn publish(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    headers: axum::http::HeaderMap,
    ApiJson(input): ApiJson<NextPublishInput>,
) -> Result<Json<NextPublication>, AppError> {
    if input.request_id.is_nil()
        || input.preview_token.len() != 64
        || !input.preview_token.bytes().all(|v| v.is_ascii_hexdigit())
    {
        return Err(language::invalid());
    }
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, &auth, "words.publish").await?;
    let access = permissions::lock(&mut tx, auth.subject).await?;
    super::repository::lock_list_writers(&mut tx).await?;
    let owner: Uuid = sqlx::query_scalar("SELECT owner_id FROM lexicon_next.entries WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("entry not found"))?;
    access.require_owned_action("words.publish", owner)?;
    let request = serde_json::json!({"entry_id":id,"input":input});
    let existing: Option<(Value, Uuid)> = sqlx::query_as("SELECT request,publication_id FROM lexicon_next.publication_commands WHERE actor_id=$1 AND request_id=$2").bind(auth.subject).bind(input.request_id).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    if let Some((old, publication)) = existing {
        if old != request {
            return Err(conflict("请求标识已用于其他发布内容"));
        }
        let row = sqlx::query("SELECT * FROM lexicon_next.publications WHERE id=$1")
            .bind(publication)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        return Ok(Json(decode(row, &headers)?));
    }
    let prepared = prepare(
        &mut tx,
        &auth,
        id,
        &NextPublicationBase {
            base_revision: input.base_revision,
            base_lifecycle_revision: input.base_lifecycle_revision,
        },
    )
    .await?;
    if prepared.preview.preview_token != input.preview_token {
        return Err(conflict("发布内容或依赖已变化，请重新检查并确认发布"));
    }
    if !prepared.preview.can_publish {
        return Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "发布检查未通过，请重新检查并补齐阻塞项",
        ));
    }
    let publication = Uuid::now_v7();
    sqlx::query("UPDATE lexicon_next.entries SET status='published',lifecycle_revision=lifecycle_revision+1 WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    let row = sqlx::query("INSERT INTO lexicon_next.publications(id,entry_id,revision,lifecycle_revision,published_by_admin_id,snapshot) VALUES($1,$2,$3,$4,$5,$6) RETURNING *").bind(publication).bind(id).bind(input.base_revision).bind(input.base_lifecycle_revision + 1).bind(auth.subject).bind(serde_json::to_value(&prepared.snapshot).map_err(AppError::internal)?).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    for (asset, resource) in prepared.assets {
        sqlx::query("INSERT INTO lexicon_next.publication_audio_references(publication_id,asset_id,resource_id) VALUES($1,$2,$3)").bind(publication).bind(asset).bind(resource).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    for taxonomy in prepared.taxonomies {
        sqlx::query("INSERT INTO lexicon_next.publication_taxonomy_refs(publication_id,taxonomy_id) VALUES($1,$2)")
            .bind(publication).bind(taxonomy).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    sqlx::query("INSERT INTO lexicon_next.publication_commands(actor_id,request_id,request,publication_id) VALUES($1,$2,$3,$4)").bind(auth.subject).bind(input.request_id).bind(request).bind(publication).execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(decode(row, &headers)?))
}
