//! Independently published sentences. Word drafts never write this content.
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use sqlx::{PgConnection, PgPool, Row};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    admin::{Admin, AdminAuth, authorization::require_active_admin},
    api::{ApiJson, ApiPath, ApiQuery},
    error::{AppError, ErrorCode},
    lexicon::{
        dto::{
            DraftMeaningsStepContentV3, SentenceSourceRangeV1, TextLinkV3, WordDefinitionV3,
            WordSentenceWritableV3,
        },
        normalization::{HEADWORD_NORMALIZATION_VERSION, normalize_headword},
        sentence_target_discovery::tokenize,
    },
    state::AppState,
};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SentenceTarget {
    Linked {
        target_entry_id: Uuid,
        target_pos_id: Uuid,
        target_base_form_id: Uuid,
        target_form_id: Uuid,
        target_variant_id: Uuid,
        target_sense_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        target_publication_id: Option<Uuid>,
    },
    /// Historical entry-only association, readable for repair but not writable.
    EntryOnly { target_entry_id: Uuid },
    Pending {
        kind: String,
        headword: String,
        gloss: Option<String>,
    },
}

impl SentenceTarget {
    pub(crate) fn as_text_link(
        &self,
        id: Uuid,
        source_segments: Vec<SentenceSourceRangeV1>,
    ) -> Option<TextLinkV3> {
        if let Self::Linked {
            target_entry_id,
            target_pos_id,
            target_base_form_id,
            target_form_id,
            target_variant_id,
            target_sense_id,
            target_publication_id,
        } = self
        {
            Some(TextLinkV3 {
                id,
                source_segments,
                target_word_id: *target_entry_id,
                target_pos_id: *target_pos_id,
                target_base_form_id: *target_base_form_id,
                target_form_id: *target_form_id,
                target_variant_id: *target_variant_id,
                target_sense_id: *target_sense_id,
                target_publication_id: *target_publication_id,
                via_phrase: None,
                target_headword: None,
                target_gloss: None,
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedSentenceAnnotation {
    pub id: Uuid,
    pub source_dialect: String,
    pub source_segments: Vec<SentenceSourceRangeV1>,
    pub target: SentenceTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedSentenceContent {
    pub sentence: WordSentenceWritableV3,
    pub annotations: Vec<SharedSentenceAnnotation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSharedSentence {
    /// Stable client-generated UUID makes retries safe.
    pub source_entry_id: Uuid,
    pub source_sense_id: Uuid,
    pub content: SharedSentenceContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSharedSentence {
    pub base_revision: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub context_entry_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub context_sense_id: Option<Uuid>,
    pub content: SharedSentenceContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceRevision {
    pub base_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UnlinkSentenceSense {
    pub base_revision: i64,
    pub sense_id: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceSenseSummary {
    pub id: Uuid,
    pub gloss: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedSentenceEntry {
    pub id: Uuid,
    pub headword: String,
    pub kind: String,
    pub senses: Vec<SentenceSenseSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedSentence {
    pub id: Uuid,
    pub revision: i64,
    pub content: SharedSentenceContent,
    pub entries: Vec<SharedSentenceEntry>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceAssociationStatus {
    Pending,
    EntryOnly,
    Unlinked,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceListSort {
    CreatedAtDesc,
    UpdatedAtDesc,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SentenceListQuery {
    /// Filter before pagination. Pending and unlinked may overlap.
    #[param(inline)]
    pub association_status: Option<SentenceAssociationStatus>,
    /// Pending annotations matching this entry's current draft forms.
    pub pending_entry_id: Option<Uuid>,
    #[param(inline)]
    pub sort: Option<SentenceListSort>,
    pub q: Option<String>,
    pub level: Option<String>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    pub entry_id: Option<Uuid>,
    pub sense_id: Option<Uuid>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedSentenceList {
    pub items: Vec<SharedSentence>,
    pub total: i64,
}

fn invalid(message: &str) -> AppError {
    AppError::validation(ErrorCode::InvalidRequestBody, "content", message)
}
fn conflict() -> AppError {
    AppError::conflict(
        ErrorCode::RevisionConflict,
        None,
        "例句或引用已变化，请刷新后重试",
    )
}
fn missing() -> AppError {
    AppError::not_found_with_code(ErrorCode::SentenceNotFound, "例句不存在或已删除")
}

// Positions are Unicode code points, matching the existing voice editor.
fn validate(content: &SharedSentenceContent) -> Result<(), AppError> {
    let sentence = &content.sentence;
    let mut issues = Vec::new();
    crate::lexicon::v3_contract::validate_english_text_limits(&sentence.en_text, &mut issues);
    if !issues.is_empty() {
        return Err(invalid("英文语音或富文本设置无效"));
    }
    if !["A1", "A2", "B1", "B2", "C1", "C2"].contains(&sentence.level.as_str()) {
        return Err(invalid("请选择例句等级"));
    }
    if !sentence.links.is_empty() {
        return Err(invalid("请通过句内标注关联具体词条"));
    }
    if sentence.zh_translations.is_empty() || sentence.zh_translations.len() > 2000 {
        return Err(invalid("至少填写一条译文"));
    }
    let mut ids = std::collections::HashSet::new();
    for translation in &sentence.zh_translations {
        if !ids.insert(translation.id)
            || translation.content.text().trim().is_empty()
            || translation.content.text().chars().count() > 10000
        {
            return Err(invalid("译文不能为空或重复"));
        }
        let rich: crate::lexicon::dto::RichText = serde_json::from_value(
            serde_json::to_value(&translation.content).map_err(AppError::internal)?,
        )
        .map_err(AppError::internal)?;
        if !crate::lexicon::rich_text::is_valid(&rich) {
            return Err(invalid("译文标注无效"));
        }
    }
    if !sentence.zh_translations.iter().any(|t| {
        t.id == sentence.zh_text_id
            && serde_json::to_value(&t.content).ok() == serde_json::to_value(&sentence.zh_text).ok()
    }) {
        return Err(invalid("译文主项不一致"));
    }
    let english = serde_json::to_value(&sentence.en_text).map_err(AppError::internal)?;
    let mut variants = std::collections::HashMap::new();
    for dialect in ["common", "uk", "us"] {
        let slot = &english[dialect];
        let variant = if dialect == "common" {
            slot
        } else {
            &slot["variant"]
        };
        if let Some(text) = variant["value"]["text"].as_str() {
            if text.trim().is_empty() || text.chars().count() > 10000 {
                return Err(invalid("英文例句不能为空或过长"));
            }
            let rich: crate::lexicon::dto::RichText =
                serde_json::from_value(variant["value"].clone())
                    .map_err(|_| invalid("英文格式无效"))?;
            if !crate::lexicon::rich_text::is_valid(&rich) {
                return Err(invalid("英文标注无效"));
            }
            if variant["text_links"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
            {
                return Err(invalid("请通过句内标注关联具体词条"));
            }
            variants.insert(dialect, text.chars().collect::<Vec<_>>());
        }
    }
    if variants.is_empty() || content.annotations.len() > 100 {
        return Err(invalid("例句或标注数量无效"));
    }
    ids.clear();
    let mut occupied = std::collections::HashSet::new();
    for a in &content.annotations {
        if !ids.insert(a.id) || a.source_segments.is_empty() || a.source_segments.len() > 20 {
            return Err(invalid("标注 ID 或片段无效"));
        }
        let text = variants
            .get(a.source_dialect.as_str())
            .ok_or_else(|| invalid("标注方言不存在"))?;
        let mut previous_end = 0;
        for segment in &a.source_segments {
            let start = segment.start;
            let end = segment.end;
            if start < previous_end
                || start >= end
                || end > text.len()
                || text[start..end].iter().collect::<String>() != segment.surface
            {
                return Err(invalid("标注位置与例句不一致，请重新选择"));
            }
            for offset in start..end {
                if !occupied.insert((a.source_dialect.as_str(), offset)) {
                    return Err(invalid("句内标注不能重叠"));
                }
            }
            previous_end = end;
        }
        let literal = selected_literal(&text.iter().collect::<String>(), &a.source_segments)?;
        if let SentenceTarget::Pending {
            kind,
            headword,
            gloss,
        } = &a.target
            && (!["word", "phrase"].contains(&kind.as_str())
                || headword.chars().count() > 200
                || normalize_headword(headword).map(|value| value.key).ok() != Some(literal)
                || gloss.as_ref().is_some_and(|s| s.chars().count() > 2000))
        {
            return Err(invalid("待关联词条信息无效"));
        }
    }
    Ok(())
}

async fn writable_entry(conn: &mut PgConnection, id: Uuid, admin: &Admin) -> Result<(), AppError> {
    let row = sqlx::query("SELECT created_by_admin_id,current_publication_id,archived_at FROM lexicon.entries WHERE id=$1 FOR SHARE")
        .bind(id).fetch_optional(&mut *conn).await.map_err(AppError::internal)?.ok_or_else(|| AppError::not_found("词条不存在"))?;
    if row.get::<Option<DateTime<Utc>>, _>("archived_at").is_some() {
        return Err(AppError::conflict(
            ErrorCode::EntryArchived,
            None,
            "词条已归档",
        ));
    }
    if row
        .get::<Option<Uuid>, _>("current_publication_id")
        .is_none()
        && row.get::<Uuid, _>("created_by_admin_id") != admin.id
        && !admin.is_super_admin()
    {
        return Err(AppError::forbidden(
            ErrorCode::EntryEditForbidden,
            "无权修改此草稿词条",
        ));
    }
    Ok(())
}

async fn lock_targets(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    content: Option<&SharedSentenceContent>,
    source: Option<Uuid>,
    sentence_id: Option<Uuid>,
) -> Result<(), AppError> {
    let mut ids: Vec<Uuid> = content
        .into_iter()
        .flat_map(|c| &c.annotations)
        .filter_map(|a| match a.target {
            SentenceTarget::Linked {
                target_entry_id, ..
            } => Some(target_entry_id),
            _ => None,
        })
        .chain(source)
        .collect();
    let required = ids.clone();
    if let Some(id) = sentence_id {
        ids.extend(sqlx::query_scalar::<_,Uuid>("SELECT target_entry_id FROM lexicon.shared_sentence_annotations WHERE sentence_id=$1 AND target_entry_id IS NOT NULL").bind(id).fetch_all(&mut **tx).await.map_err(AppError::internal)?);
    }
    ids.sort_unstable();
    ids.dedup();
    crate::lexicon::repository::LexiconRepository::lock_surface_contexts(tx, &ids)
        .await
        .map_err(|error| match error {
            crate::lexicon::repository::LexiconRepositoryError::SurfaceContextBusy => {
                AppError::conflict(
                    ErrorCode::ReferenceConflict,
                    None,
                    "词条正在被修改，请稍后重试",
                )
            }
            other => AppError::internal(other),
        })?;
    let rows: Vec<(Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT id,archived_at FROM lexicon.entries WHERE id=ANY($1) ORDER BY id FOR SHARE",
    )
    .bind(&ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    if required.iter().any(|id| {
        !rows
            .iter()
            .any(|(row_id, archived)| row_id == id && archived.is_none())
    }) {
        return Err(invalid("关联词条不存在或已归档"));
    }
    Ok(())
}

async fn annotations(
    conn: &mut PgConnection,
    id: Uuid,
    content: &SharedSentenceContent,
) -> Result<(), AppError> {
    for a in &content.annotations {
        let (target, kind, headword, normalized, gloss) = match &a.target {
            SentenceTarget::Linked {
                target_entry_id, ..
            }
            | SentenceTarget::EntryOnly { target_entry_id } => {
                (Some(*target_entry_id), None, None, None, None)
            }
            SentenceTarget::Pending {
                kind,
                headword,
                gloss,
            } => (
                None,
                Some(kind.clone()),
                Some(headword.clone()),
                Some(
                    normalize_headword(headword)
                        .map_err(|_| invalid("待关联词面无效"))?
                        .key,
                ),
                gloss.clone(),
            ),
        };
        sqlx::query("INSERT INTO lexicon.shared_sentence_annotations(sentence_id,id,source_dialect,source_segments,target_entry_id,pending_kind,pending_headword,pending_normalized,pending_gloss,target_sense_id,target_ref) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(id).bind(a.id).bind(&a.source_dialect).bind(serde_json::to_value(&a.source_segments).map_err(AppError::internal)?)
            .bind(target).bind(kind).bind(headword).bind(normalized).bind(gloss)
            .bind(a.target.as_text_link(a.id,vec![]).map(|l|l.target_sense_id))
            .bind(if matches!(a.target,SentenceTarget::Linked{..}) {Some(serde_json::to_value(&a.target).map_err(AppError::internal)?)} else {None}).execute(&mut *conn).await.map_err(AppError::internal)?;
    }
    Ok(())
}

async fn read(pool: &PgPool, id: Uuid) -> Result<SharedSentence, AppError> {
    let mut snapshot = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *snapshot)
        .await
        .map_err(AppError::internal)?;
    read_on(&mut snapshot, id).await
}

async fn read_on(snapshot: &mut PgConnection, id: Uuid) -> Result<SharedSentence, AppError> {
    let row = sqlx::query("SELECT s.*,a.display_name AS created_by FROM lexicon.shared_sentences s JOIN admins a ON a.id=s.created_by_admin_id WHERE s.id=$1 AND s.deleted_at IS NULL")
        .bind(id).fetch_optional(&mut *snapshot).await.map_err(AppError::internal)?.ok_or_else(missing)?;
    let ann = sqlx::query("SELECT * FROM lexicon.shared_sentence_annotations WHERE sentence_id=$1 ORDER BY source_dialect, source_segments->0->>'start',id").bind(id).fetch_all(&mut *snapshot).await.map_err(AppError::internal)?;
    let annotations = ann
        .into_iter()
        .map(|a| {
            let target =
                if let Some(reference) = a.get::<Option<serde_json::Value>, _>("target_ref") {
                    serde_json::from_value(reference).map_err(AppError::internal)?
                } else if let Some(target_entry_id) = a.get::<Option<Uuid>, _>("target_entry_id") {
                    SentenceTarget::EntryOnly { target_entry_id }
                } else {
                    SentenceTarget::Pending {
                        kind: a.get("pending_kind"),
                        headword: a.get("pending_headword"),
                        gloss: a.get("pending_gloss"),
                    }
                };
            Ok(SharedSentenceAnnotation {
                id: a.get("id"),
                source_dialect: a.get("source_dialect"),
                source_segments: serde_json::from_value(a.get("source_segments"))
                    .map_err(AppError::internal)?,
                target,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let mut entries: Vec<SharedSentenceEntry> = sqlx::query("SELECT e.id,e.kind,COALESCE((SELECT label FROM lexicon.entry_presentation_projection p WHERE p.entry_id=e.id),'') AS headword FROM lexicon.entries e WHERE e.id IN (SELECT target_entry_id FROM lexicon.shared_sentence_annotations WHERE sentence_id=$1) ORDER BY headword,e.id")
        .bind(id).fetch_all(&mut *snapshot).await.map_err(AppError::internal)?.into_iter().map(|r| SharedSentenceEntry {id:r.get("id"),headword:r.get("headword"),kind:r.get("kind"),senses:vec![]}).collect();
    let summaries=sqlx::query("SELECT DISTINCT a.target_entry_id,a.target_sense_id,COALESCE(pub.snapshot->'meanings',p.meanings) AS meanings FROM lexicon.shared_sentence_annotations a LEFT JOIN lexicon.entry_editor_projection p ON p.entry_id=a.target_entry_id LEFT JOIN lexicon.entry_publications pub ON pub.entry_id=a.target_entry_id AND pub.id=(a.target_ref->>'target_publication_id')::uuid WHERE a.sentence_id=$1 AND a.target_sense_id IS NOT NULL")
        .bind(id).fetch_all(&mut *snapshot).await.map_err(AppError::internal)?;
    for row in summaries {
        let sense_id: Uuid = row.get("target_sense_id");
        let meanings: Option<DraftMeaningsStepContentV3> = row
            .get::<Option<serde_json::Value>, _>("meanings")
            .and_then(|v| serde_json::from_value(v).ok());
        let gloss = meanings
            .as_ref()
            .and_then(|m| {
                m.pos
                    .iter()
                    .flat_map(|p| &p.senses)
                    .find(|s| s.id == sense_id)
            })
            .and_then(|s| {
                s.definitions.iter().find_map(|d| match d {
                    WordDefinitionV3::ZhDefinition { content, .. }
                    | WordDefinitionV3::ZhSentence { content, .. } => {
                        Some(content.text().to_owned())
                    }
                    _ => None,
                })
            })
            .unwrap_or_default();
        if let Some(entry) = entries
            .iter_mut()
            .find(|e| e.id == row.get::<Uuid, _>("target_entry_id"))
            && !entry.senses.iter().any(|s| s.id == sense_id)
        {
            entry.senses.push(SentenceSenseSummary {
                id: sense_id,
                gloss,
            });
        }
    }
    Ok(SharedSentence {
        id,
        revision: row.get("revision"),
        content: SharedSentenceContent {
            sentence: serde_json::from_value(row.get("content")).map_err(AppError::internal)?,
            annotations,
        },
        entries,
        created_by: row.get("created_by"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

async fn lock(conn: &mut PgConnection, id: Uuid, revision: i64) -> Result<(), AppError> {
    let current: Option<i64> = sqlx::query_scalar("SELECT revision FROM lexicon.shared_sentences WHERE id=$1 AND deleted_at IS NULL FOR UPDATE").bind(id).fetch_optional(conn).await.map_err(AppError::internal)?;
    if current.ok_or_else(missing)? != revision {
        return Err(conflict());
    }
    Ok(())
}
async fn bump(conn: &mut PgConnection, id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE lexicon.shared_sentences SET revision=revision+1,updated_at=now() WHERE id=$1",
    )
    .bind(id)
    .execute(conn)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}

#[utoipa::path(get,path="/api/v1/admin/lexicon/sentences",tag="admin-lexicon",security(("bearer_auth"=[])),params(SentenceListQuery),responses((status=200,body=SharedSentenceList),(status=400,description="查询参数无效"),(status=401,description="未登录"),(status=403,description="管理员不可用或无权编辑目标词条")))]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(q): ApiQuery<SentenceListQuery>,
) -> Result<Json<SharedSentenceList>, AppError> {
    require_active_admin(&state, &auth).await?;
    let page = q.page.unwrap_or(1);
    let size = q.page_size.unwrap_or(10);
    if (q.sense_id.is_some() && q.entry_id.is_none())
        || (q.pending_entry_id.is_some() && (q.entry_id.is_some() || q.sense_id.is_some()))
        || !(1..=100000).contains(&page)
        || !(1..=50).contains(&size)
        || q.q.as_ref().is_some_and(|s| s.chars().count() > 200)
        || q.created_from.zip(q.created_to).is_some_and(|(a, b)| a > b)
    {
        return Err(AppError::validation(
            ErrorCode::InvalidQuery,
            "query",
            "分页或查询参数无效",
        ));
    }
    let mut snapshot = state.pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *snapshot)
        .await
        .map_err(AppError::internal)?;
    let association_status = q.association_status.map(|status| match status {
        SentenceAssociationStatus::Pending => "pending",
        SentenceAssociationStatus::EntryOnly => "entry_only",
        SentenceAssociationStatus::Unlinked => "unlinked",
    });
    let pending_forms = if let Some(entry) = q.pending_entry_id {
        let targets = target_forms(&mut snapshot, Some(entry), Some(entry), None).await?;
        let target = targets
            .items
            .first()
            .ok_or_else(|| invalid("目标词条不存在、已归档或没有可匹配词形"))?;
        let forms = target.surfaces.iter().map(|surface| {
            Ok(serde_json::json!({
                "kind": target.kind,
                "normalized": normalize_headword(&surface.surface).map_err(|_| invalid("目标词形无效"))?.key,
                "dialect": surface.dialect,
            }))
        }).collect::<Result<Vec<_>, AppError>>()?;
        Some(serde_json::Value::Array(forms))
    } else {
        None
    };
    macro_rules! sentence_filter { () => { r#" FROM lexicon.shared_sentences s JOIN admins creator ON creator.id=s.created_by_admin_id
        WHERE s.deleted_at IS NULL
        AND ($1::text IS NULL OR s.id::text ILIKE '%'||$1||'%' OR s.content::text ILIKE '%'||$1||'%' OR creator.display_name ILIKE '%'||$1||'%')
        AND ($2::text IS NULL OR s.content->>'level'=$2)
        AND ($3::timestamptz IS NULL OR s.created_at >= $3) AND ($4::timestamptz IS NULL OR s.created_at <= $4)
        AND ($5::uuid IS NULL OR EXISTS(SELECT 1 FROM lexicon.shared_sentence_annotations a WHERE a.sentence_id=s.id AND a.target_entry_id=$5 AND a.target_sense_id IS NOT NULL AND ($6::uuid IS NULL OR a.target_sense_id=$6)))
        AND ($7::text IS NULL
          OR ($7='pending' AND EXISTS(SELECT 1 FROM lexicon.shared_sentence_annotations a WHERE a.sentence_id=s.id AND a.pending_kind IS NOT NULL))
          OR ($7='entry_only' AND EXISTS(SELECT 1 FROM lexicon.shared_sentence_annotations a WHERE a.sentence_id=s.id AND a.target_entry_id IS NOT NULL AND a.target_sense_id IS NULL))
          OR ($7='unlinked' AND NOT EXISTS(SELECT 1 FROM lexicon.shared_sentence_annotations a WHERE a.sentence_id=s.id AND a.target_entry_id IS NOT NULL)))
        AND ($8::jsonb IS NULL OR EXISTS(
          SELECT 1 FROM lexicon.shared_sentence_annotations a
          JOIN jsonb_to_recordset($8) AS f(kind text, normalized text, dialect text)
            ON a.pending_kind=f.kind AND a.pending_normalized=f.normalized
            AND (a.source_dialect='common' OR a.source_dialect=f.dialect)
          WHERE a.sentence_id=s.id))"# }; }
    let total: i64 = sqlx::query_scalar(concat!("SELECT count(*) ", sentence_filter!()))
        .bind(&q.q)
        .bind(&q.level)
        .bind(q.created_from)
        .bind(q.created_to)
        .bind(q.entry_id)
        .bind(q.sense_id)
        .bind(association_status)
        .bind(&pending_forms)
        .fetch_one(&mut *snapshot)
        .await
        .map_err(AppError::internal)?;
    let ids: Vec<Uuid> = sqlx::query_scalar(concat!(
        "SELECT s.id ",
        sentence_filter!(),
        " ORDER BY CASE WHEN $9 THEN s.updated_at ELSE s.created_at END DESC,s.id LIMIT $10 OFFSET $11"
    ))
    .bind(&q.q)
    .bind(&q.level)
    .bind(q.created_from)
    .bind(q.created_to)
    .bind(q.entry_id)
    .bind(q.sense_id)
        .bind(association_status)
        .bind(&pending_forms)
    .bind(matches!(q.sort, Some(SentenceListSort::UpdatedAtDesc)))
    .bind(size)
    .bind((page - 1) * size)
    .fetch_all(&mut *snapshot)
    .await
    .map_err(AppError::internal)?;
    let mut items = Vec::new();
    for id in ids {
        match read_on(&mut snapshot, id).await {
            Ok(item) => items.push(item),
            Err(e) => return Err(e),
        }
    }
    Ok(Json(SharedSentenceList { items, total }))
}

#[utoipa::path(get,path="/api/v1/admin/lexicon/sentences/{id}",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,body=SharedSentence),(status=400,description="内容或目标无效"),(status=401,description="未登录"),(status=403,description="管理员不可用或无权编辑目标词条"),(status=404,description="例句或词条不存在"),(status=409,description="版本或幂等冲突"),(status=422,description="请求结构无效")))]
pub async fn get(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<SharedSentence>, AppError> {
    require_active_admin(&state, &auth).await?;
    Ok(Json(read(&state.pool, id).await?))
}

#[utoipa::path(post,path="/api/v1/admin/lexicon/sentences",tag="admin-lexicon",security(("bearer_auth"=[])),request_body=CreateSharedSentence,responses((status=200,body=SharedSentence),(status=400,description="内容或目标无效"),(status=401,description="未登录"),(status=403,description="管理员不可用或无权编辑目标词条"),(status=404,description="例句或词条不存在"),(status=409,description="版本或幂等冲突"),(status=422,description="请求结构无效")))]
pub async fn create(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<CreateSharedSentence>,
) -> Result<Json<SharedSentence>, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    validate(&input.content)?;
    let id = input.content.sentence.id;
    let payload = {
        use sha2::{Digest, Sha256};
        Sha256::digest(serde_json::to_vec(&input).map_err(AppError::internal)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock_targets(
        &mut tx,
        Some(&input.content),
        Some(input.source_entry_id),
        None,
    )
    .await?;
    writable_entry(&mut tx, input.source_entry_id, &admin).await?;
    validate_targets(
        &mut tx,
        &input.content,
        Some(input.source_entry_id),
        Some(input.source_sense_id),
    )
    .await?;
    let inserted=sqlx::query("INSERT INTO lexicon.shared_sentences(id,content,create_digest,source_entry_id,created_by_admin_id) VALUES($1,$2,$3,$4,$5) ON CONFLICT(id) DO NOTHING")
        .bind(id).bind(serde_json::to_value(&input.content.sentence).map_err(AppError::internal)?).bind(&payload).bind(input.source_entry_id).bind(auth.subject).execute(&mut *tx).await.map_err(AppError::internal)?.rows_affected();
    if inserted == 0 {
        let original=sqlx::query("SELECT create_digest,created_by_admin_id,deleted_at FROM lexicon.shared_sentences WHERE id=$1 FOR UPDATE").bind(id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
        if original.get::<String, _>("create_digest") != payload
            || original.get::<Uuid, _>("created_by_admin_id") != auth.subject
        {
            return Err(AppError::conflict(
                ErrorCode::IdempotencyConflict,
                None,
                "此创建 ID 已用于其他内容",
            ));
        }
        if original
            .get::<Option<DateTime<Utc>>, _>("deleted_at")
            .is_some()
        {
            return Err(missing());
        }
    } else {
        annotations(&mut tx, id, &input.content).await?;
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(read(&state.pool, id).await?))
}

#[utoipa::path(put,path="/api/v1/admin/lexicon/sentences/{id}",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=UpdateSharedSentence,responses((status=200,body=SharedSentence),(status=400,description="内容或目标无效"),(status=401,description="未登录"),(status=403,description="管理员不可用或无权编辑目标词条"),(status=404,description="例句或词条不存在"),(status=409,description="版本或幂等冲突"),(status=422,description="请求结构无效")))]
pub async fn update(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<UpdateSharedSentence>,
) -> Result<Json<SharedSentence>, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    validate(&input.content)?;
    if input.context_entry_id.is_some() != input.context_sense_id.is_some() {
        return Err(invalid("当前词条与词义必须同时提供"));
    }
    if id != input.content.sentence.id {
        return Err(invalid("例句 ID 不可更改"));
    }
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock_targets(
        &mut tx,
        Some(&input.content),
        input.context_entry_id,
        Some(id),
    )
    .await?;
    if let Some(context) = input.context_entry_id {
        writable_entry(&mut tx, context, &admin).await?;
    }
    validate_targets(
        &mut tx,
        &input.content,
        input.context_entry_id,
        input.context_sense_id,
    )
    .await?;
    lock(&mut tx, id, input.base_revision).await?;
    sqlx::query("DELETE FROM lexicon.shared_sentence_annotations WHERE sentence_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    annotations(&mut tx, id, &input.content).await?;
    sqlx::query("UPDATE lexicon.shared_sentences SET content=$2 WHERE id=$1")
        .bind(id)
        .bind(serde_json::to_value(&input.content.sentence).map_err(AppError::internal)?)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    bump(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(read(&state.pool, id).await?))
}

#[utoipa::path(delete,path="/api/v1/admin/lexicon/sentences/{id}",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=SentenceRevision,responses((status=204,description="删除或移除收录成功"),(status=400,description="输入无效"),(status=401,description="未登录"),(status=403,description="管理员不可用或无权编辑目标词条"),(status=404,description="例句或词条不存在"),(status=409,description="版本冲突"),(status=422,description="请求结构无效")))]
pub async fn delete(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<SentenceRevision>,
) -> Result<StatusCode, AppError> {
    require_active_admin(&state, &auth).await?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock(&mut tx, id, input.base_revision).await?;
    sqlx::query("UPDATE lexicon.shared_sentences SET deleted_at=now(),updated_at=now(),revision=revision+1 WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    sqlx::query("DELETE FROM lexicon.shared_sentence_annotations WHERE sentence_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("DELETE FROM lexicon.shared_sentence_collections WHERE sentence_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete,path="/api/v1/admin/lexicon/sentences/{id}/associations/{entry_id}",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path),("entry_id"=Uuid,Path)),request_body=UnlinkSentenceSense,responses((status=204,description="解除当前词义全部关联"),(status=400,description="输入无效"),(status=401,description="未登录"),(status=403,description="无权编辑目标词条"),(status=404,description="例句或词条不存在"),(status=409,description="版本冲突"),(status=422,description="请求结构无效")))]
pub async fn unlink(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath((id, entry_id)): ApiPath<(Uuid, Uuid)>,
    ApiJson(input): ApiJson<UnlinkSentenceSense>,
) -> Result<StatusCode, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock_targets(&mut tx, None, Some(entry_id), Some(id)).await?;
    writable_entry(&mut tx, entry_id, &admin).await?;
    lock(&mut tx, id, input.base_revision).await?;
    sqlx::query("DELETE FROM lexicon.shared_sentence_annotations WHERE sentence_id=$1 AND target_entry_id=$2 AND target_sense_id=$3")
        .bind(id).bind(entry_id).bind(input.sense_id).execute(&mut *tx).await.map_err(AppError::internal)?;
    bump(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

fn selected_literal(text: &str, segments: &[SentenceSourceRangeV1]) -> Result<String, AppError> {
    let tokens = tokenize(text);
    let first = segments
        .first()
        .ok_or_else(|| invalid("请选择完整单词或短语"))?;
    let last = segments.last().unwrap();
    for segment in segments {
        if !tokens.iter().any(|t| t.range.start == segment.start)
            || !tokens.iter().any(|t| t.range.end == segment.end)
        {
            return Err(invalid("请选择完整单词，不能截取单词的一部分"));
        }
    }
    if tokens
        .iter()
        .any(|t| t.range.start > first.start && t.range.start < last.end && t.hard_boundary_before)
    {
        return Err(invalid("关联短语不能跨越句子边界"));
    }
    normalize_headword(
        &segments
            .iter()
            .map(|s| s.surface.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    )
    .map(|value| value.key)
    .map_err(|_| invalid("所选文字不能构成单词或短语"))
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceTargetSurface {
    pub surface: String,
    pub dialect: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceEntryTarget {
    pub id: Uuid,
    pub headword: String,
    pub kind: String,
    pub surfaces: Vec<SentenceTargetSurface>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceEntryTargets {
    pub items: Vec<SentenceEntryTarget>,
    pub total: i64,
}
#[derive(Debug, Deserialize, IntoParams)]
pub struct SentenceTargetQuery {
    pub entry_id: Option<Uuid>,
    pub context_entry_id: Option<Uuid>,
    pub q: Option<String>,
    pub kind: Option<String>,
    pub dialect: Option<String>,
    pub page: Option<i64>,
}

// Both candidate discovery and write validation use the same effective forms.
// Initial keys only belong to an unfilled skeleton; they cannot revive a changed lemma.
const TARGET_FORMS: &str = r#"
WITH effective AS NOT MATERIALIZED (
    SELECT e.id, e.kind, COALESCE(p.label, '') AS headword,
           CASE WHEN e.id=$1 OR e.current_publication_id IS NULL THEN 'draft' ELSE 'current_publication' END AS scope,
           e.current_publication_id
    FROM lexicon.entries e
    LEFT JOIN lexicon.entry_presentation_projection p ON p.entry_id=e.id
    WHERE e.archived_at IS NULL AND e.language='en' AND e.kind IN ('word','phrase')
      AND ($3::uuid IS NULL OR e.id=$3)
      AND ($5::text IS NULL OR e.kind=$5)
), forms AS (
    SELECT e.id,e.kind,e.headword,s.surface,s.normalized_surface,s.dialect_scope AS dialect
    FROM effective e JOIN lexicon.surface_sources s ON s.entry_id=e.id
    WHERE NOT s.is_deleted AND s.language='en' AND s.normalization_version=$2
      AND s.content_scope=e.scope
      AND (e.scope='draft' OR s.publication_id=e.current_publication_id)
      AND ($4::text IS NULL OR s.normalized_surface=$4)
      AND ($6::text IS NULL OR $6='common' OR s.dialect_scope=$6)
    UNION
    SELECT e.id,e.kind,e.headword,substring(k FROM 4),substring(k FROM 4),left(k,2)
    FROM effective e JOIN lexicon.v3_entry_state st ON st.entry_id=e.id
    JOIN lexicon.entry_editor_projection p ON p.entry_id=e.id
    CROSS JOIN LATERAL unnest(st.initial_headword_keys) k
    WHERE e.scope='draft' AND COALESCE(jsonb_array_length(p.forms->'pos'),0)=0
      AND NOT EXISTS(SELECT 1 FROM lexicon.surface_sources s WHERE s.entry_id=e.id AND NOT s.is_deleted AND s.content_scope='draft')
      AND ($4::text IS NULL OR substring(k FROM 4)=$4)
      AND ($6::text IS NULL OR $6='common' OR left(k,2)=$6)
), matched AS (
    SELECT DISTINCT id,kind,headword FROM forms
), page AS (
    SELECT * FROM matched ORDER BY headword,id LIMIT 50 OFFSET $7
)
SELECT (SELECT count(*) FROM matched)::bigint AS total,
    COALESCE(jsonb_agg(jsonb_build_object('id',p.id,'kind',p.kind,'headword',p.headword,
      'surfaces',(SELECT jsonb_agg(jsonb_build_object('surface',f.surface,'dialect',f.dialect) ORDER BY f.dialect,f.surface) FROM forms f WHERE f.id=p.id)) ORDER BY p.headword,p.id),'[]'::jsonb) AS items
FROM page p
"#;

async fn target_forms(
    conn: &mut PgConnection,
    context: Option<Uuid>,
    entry: Option<Uuid>,
    query: Option<&SentenceTargetQuery>,
) -> Result<SentenceEntryTargets, AppError> {
    let normalized = query
        .and_then(|q| q.q.as_ref())
        .map(|s| normalize_headword(s).map(|v| v.key))
        .transpose()
        .map_err(|_| invalid("所选词面无效"))?;
    let row = sqlx::query(TARGET_FORMS)
        .bind(context)
        .bind(HEADWORD_NORMALIZATION_VERSION)
        .bind(entry)
        .bind(normalized)
        .bind(query.and_then(|q| q.kind.as_deref()))
        .bind(query.and_then(|q| q.dialect.as_deref()))
        .bind((query.and_then(|q| q.page).unwrap_or(1) - 1) * 50)
        .fetch_one(conn)
        .await
        .map_err(AppError::internal)?;
    Ok(SentenceEntryTargets {
        total: row.get("total"),
        items: serde_json::from_value(row.get("items")).map_err(AppError::internal)?,
    })
}

async fn validate_targets(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    content: &SharedSentenceContent,
    context: Option<Uuid>,
    context_sense: Option<Uuid>,
) -> Result<(), AppError> {
    if context.is_some_and(|entry| !content.annotations.iter().any(|a| matches!(a.target,SentenceTarget::Linked{target_entry_id,target_sense_id,..} if target_entry_id==entry && Some(target_sense_id)==context_sense))) {
        return Err(invalid("请先将句中的单词或短语关联到当前词义"));
    }
    for annotation in &content.annotations {
        match &annotation.target {
            SentenceTarget::EntryOnly { .. } => {
                return Err(invalid("旧关联尚未选择具体词义，请补全或清除"));
            }
            SentenceTarget::Pending { .. } => {}
            SentenceTarget::Linked { .. } => {
                let link = annotation
                    .target
                    .as_text_link(annotation.id, annotation.source_segments.clone())
                    .unwrap();
                let valid = crate::lexicon::service::text_links::validate_shared_sentence_target(
                    tx,
                    &link,
                    &annotation.source_dialect,
                )
                .await
                .map_err(crate::lexicon::handler::map_error)?;
                if !valid {
                    return Err(invalid(
                        "关联词义或词形已失效，请按词条、词形、词义重新选择",
                    ));
                }
            }
        }
    }
    Ok(())
}

#[utoipa::path(get,path="/api/v1/admin/lexicon/sentences/targets",tag="admin-lexicon",security(("bearer_auth"=[])),params(SentenceTargetQuery),responses((status=200,body=SentenceEntryTargets),(status=400,description="查询参数无效"),(status=401,description="未登录"),(status=403,description="无权编辑词条"),(status=404,description="词条不存在"),(status=409,description="词条已归档")))]
pub async fn targets(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(q): ApiQuery<SentenceTargetQuery>,
) -> Result<Json<SentenceEntryTargets>, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let page = q.page.unwrap_or(1);
    if !(1..=100000).contains(&page)
        || (q.entry_id.is_none() && q.q.is_none())
        || q.kind
            .as_ref()
            .is_some_and(|k| !["word", "phrase"].contains(&k.as_str()))
        || q.dialect
            .as_ref()
            .is_some_and(|d| !["common", "uk", "us"].contains(&d.as_str()))
    {
        return Err(invalid("词条查询参数无效"));
    }
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    if let Some(id) = q.context_entry_id {
        writable_entry(&mut tx, id, &admin).await?;
    }
    Ok(Json(
        target_forms(&mut tx, q.context_entry_id, q.entry_id, Some(&q)).await?,
    ))
}
