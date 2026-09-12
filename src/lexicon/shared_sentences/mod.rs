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
        dto::{SentenceSourceRangeV1, WordSentenceWritableV3},
        normalization::normalize_headword,
    },
    state::AppState,
};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SentenceTarget {
    Linked {
        target_entry_id: Uuid,
    },
    Pending {
        kind: String,
        headword: String,
        gloss: Option<String>,
    },
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
    pub content: SharedSentenceContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSharedSentence {
    pub base_revision: i64,
    pub content: SharedSentenceContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceRevision {
    pub base_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectSharedSentence {
    pub base_revision: i64,
    pub entry_id: Uuid,
    /// Only pending annotations selected by the user are bound to entry_id.
    pub annotation_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedSentenceEntry {
    pub id: Uuid,
    pub headword: String,
    pub kind: String,
    pub collected: bool,
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

#[derive(Debug, Deserialize, IntoParams)]
pub struct SentenceListQuery {
    pub q: Option<String>,
    pub level: Option<String>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    pub entry_id: Option<Uuid>,
    pub candidates: Option<bool>,
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
        if let SentenceTarget::Pending {
            kind,
            headword,
            gloss,
        } = &a.target
            && (!["word", "phrase"].contains(&kind.as_str())
                || headword.chars().count() > 200
                || normalize_headword(headword).is_err()
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
) -> Result<(), AppError> {
    let mut ids: Vec<Uuid> = content
        .into_iter()
        .flat_map(|c| &c.annotations)
        .filter_map(|a| match a.target {
            SentenceTarget::Linked { target_entry_id } => Some(target_entry_id),
            _ => None,
        })
        .chain(source)
        .collect();
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
    if rows.len() != ids.len() || rows.iter().any(|(_, archived)| archived.is_some()) {
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
            SentenceTarget::Linked { target_entry_id } => {
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
        sqlx::query("INSERT INTO lexicon.shared_sentence_annotations(sentence_id,id,source_dialect,source_segments,target_entry_id,pending_kind,pending_headword,pending_normalized,pending_gloss) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(id).bind(a.id).bind(&a.source_dialect).bind(serde_json::to_value(&a.source_segments).map_err(AppError::internal)?)
            .bind(target).bind(kind).bind(headword).bind(normalized).bind(gloss).execute(&mut *conn).await.map_err(AppError::internal)?;
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
            let target = if let Some(target_entry_id) = a.get::<Option<Uuid>, _>("target_entry_id")
            {
                SentenceTarget::Linked { target_entry_id }
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
    let entries = sqlx::query("SELECT e.id,e.kind,COALESCE((SELECT label FROM lexicon.entry_presentation_projection p WHERE p.entry_id=e.id),'') AS headword,EXISTS(SELECT 1 FROM lexicon.shared_sentence_collections c WHERE c.sentence_id=$1 AND c.entry_id=e.id) AS collected FROM lexicon.entries e WHERE e.id IN (SELECT target_entry_id FROM lexicon.shared_sentence_annotations WHERE sentence_id=$1 UNION SELECT entry_id FROM lexicon.shared_sentence_collections WHERE sentence_id=$1) ORDER BY headword,e.id")
        .bind(id).fetch_all(&mut *snapshot).await.map_err(AppError::internal)?.into_iter().map(|r| SharedSentenceEntry {id:r.get("id"),headword:r.get("headword"),kind:r.get("kind"),collected:r.get("collected")}).collect();
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
    let admin = require_active_admin(&state, &auth).await?;
    let page = q.page.unwrap_or(1);
    let size = q.page_size.unwrap_or(10);
    if !(1..=100000).contains(&page)
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
    if let Some(id) = q.entry_id.filter(|_| q.candidates.unwrap_or(false)) {
        let mut conn = state.pool.acquire().await.map_err(AppError::internal)?;
        writable_entry(&mut conn, id, &admin).await?;
    }

    let mut snapshot = state.pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *snapshot)
        .await
        .map_err(AppError::internal)?;
    let total: i64=sqlx::query_scalar(concat!("SELECT count(*) ", r#" FROM lexicon.shared_sentences s JOIN admins creator ON creator.id=s.created_by_admin_id
        WHERE s.deleted_at IS NULL
        AND ($1::text IS NULL OR s.id::text ILIKE '%'||$1||'%' OR s.content::text ILIKE '%'||$1||'%' OR creator.display_name ILIKE '%'||$1||'%')
        AND ($2::text IS NULL OR s.content->>'level'=$2)
        AND ($3::timestamptz IS NULL OR s.created_at >= $3) AND ($4::timestamptz IS NULL OR s.created_at <= $4)
        AND ($5::uuid IS NULL OR (NOT $6 AND EXISTS(SELECT 1 FROM lexicon.shared_sentence_collections c WHERE c.sentence_id=s.id AND c.entry_id=$5))
        OR ($6 AND NOT EXISTS(SELECT 1 FROM lexicon.shared_sentence_collections c WHERE c.sentence_id=s.id AND c.entry_id=$5)
            AND EXISTS(SELECT 1 FROM lexicon.shared_sentence_annotations a WHERE a.sentence_id=s.id AND
                (a.target_entry_id=$5 OR (a.target_entry_id IS NULL AND EXISTS(SELECT 1 FROM lexicon.entries e WHERE e.id=$5 AND e.kind=a.pending_kind AND (EXISTS(SELECT 1 FROM lexicon.surface_sources sf WHERE sf.entry_id=e.id AND NOT sf.is_deleted AND sf.normalized_surface=a.pending_normalized) OR EXISTS(SELECT 1 FROM lexicon.v3_entry_state st WHERE st.entry_id=e.id AND ('uk:'||a.pending_normalized=ANY(st.initial_headword_keys) OR 'us:'||a.pending_normalized=ANY(st.initial_headword_keys))))))))))"#)).bind(&q.q).bind(&q.level).bind(q.created_from).bind(q.created_to).bind(q.entry_id).bind(q.candidates.unwrap_or(false)).fetch_one(&mut *snapshot).await.map_err(AppError::internal)?;
    let ids:Vec<Uuid>=sqlx::query_scalar(concat!("SELECT s.id ", r#" FROM lexicon.shared_sentences s JOIN admins creator ON creator.id=s.created_by_admin_id
        WHERE s.deleted_at IS NULL
        AND ($1::text IS NULL OR s.id::text ILIKE '%'||$1||'%' OR s.content::text ILIKE '%'||$1||'%' OR creator.display_name ILIKE '%'||$1||'%')
        AND ($2::text IS NULL OR s.content->>'level'=$2)
        AND ($3::timestamptz IS NULL OR s.created_at >= $3) AND ($4::timestamptz IS NULL OR s.created_at <= $4)
        AND ($5::uuid IS NULL OR (NOT $6 AND EXISTS(SELECT 1 FROM lexicon.shared_sentence_collections c WHERE c.sentence_id=s.id AND c.entry_id=$5))
        OR ($6 AND NOT EXISTS(SELECT 1 FROM lexicon.shared_sentence_collections c WHERE c.sentence_id=s.id AND c.entry_id=$5)
            AND EXISTS(SELECT 1 FROM lexicon.shared_sentence_annotations a WHERE a.sentence_id=s.id AND
                (a.target_entry_id=$5 OR (a.target_entry_id IS NULL AND EXISTS(SELECT 1 FROM lexicon.entries e WHERE e.id=$5 AND e.kind=a.pending_kind AND (EXISTS(SELECT 1 FROM lexicon.surface_sources sf WHERE sf.entry_id=e.id AND NOT sf.is_deleted AND sf.normalized_surface=a.pending_normalized) OR EXISTS(SELECT 1 FROM lexicon.v3_entry_state st WHERE st.entry_id=e.id AND ('uk:'||a.pending_normalized=ANY(st.initial_headword_keys) OR 'us:'||a.pending_normalized=ANY(st.initial_headword_keys))))))))))"#, " ORDER BY s.created_at DESC,s.id LIMIT $7 OFFSET $8")).bind(&q.q).bind(&q.level).bind(q.created_from).bind(q.created_to).bind(q.entry_id).bind(q.candidates.unwrap_or(false)).bind(size).bind((page-1)*size).fetch_all(&mut *snapshot).await.map_err(AppError::internal)?;
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
    lock_targets(&mut tx, Some(&input.content), Some(input.source_entry_id)).await?;
    writable_entry(&mut tx, input.source_entry_id, &admin).await?;
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
        sqlx::query(
            "INSERT INTO lexicon.shared_sentence_collections(sentence_id,entry_id) VALUES($1,$2)",
        )
        .bind(id)
        .bind(input.source_entry_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
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
    require_active_admin(&state, &auth).await?;
    validate(&input.content)?;
    if id != input.content.sentence.id {
        return Err(invalid("例句 ID 不可更改"));
    }
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock_targets(&mut tx, Some(&input.content), None).await?;
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

#[utoipa::path(post,path="/api/v1/admin/lexicon/sentences/{id}/collections",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=CollectSharedSentence,responses((status=200,body=SharedSentence),(status=400,description="内容或目标无效"),(status=401,description="未登录"),(status=403,description="管理员不可用或无权编辑目标词条"),(status=404,description="例句或词条不存在"),(status=409,description="版本或幂等冲突"),(status=422,description="请求结构无效")))]
pub async fn collect(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<CollectSharedSentence>,
) -> Result<Json<SharedSentence>, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    if input.annotation_ids.len() > 100 {
        return Err(invalid("标注过多"));
    }
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock_targets(&mut tx, None, Some(input.entry_id)).await?;
    writable_entry(&mut tx, input.entry_id, &admin).await?;
    lock(&mut tx, id, input.base_revision).await?;
    let mut eligible:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon.shared_sentence_annotations WHERE sentence_id=$1 AND target_entry_id=$2)").bind(id).bind(input.entry_id).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    for annotation_id in &input.annotation_ids {
        let changed=sqlx::query("UPDATE lexicon.shared_sentence_annotations a SET target_entry_id=$3,pending_kind=NULL,pending_headword=NULL,pending_normalized=NULL,pending_gloss=NULL WHERE sentence_id=$1 AND id=$2 AND target_entry_id IS NULL AND EXISTS(SELECT 1 FROM lexicon.entries e WHERE e.id=$3 AND e.kind=a.pending_kind AND (EXISTS(SELECT 1 FROM lexicon.surface_sources sf WHERE sf.entry_id=e.id AND NOT sf.is_deleted AND sf.normalized_surface=a.pending_normalized) OR EXISTS(SELECT 1 FROM lexicon.v3_entry_state st WHERE st.entry_id=e.id AND ('uk:'||a.pending_normalized=ANY(st.initial_headword_keys) OR 'us:'||a.pending_normalized=ANY(st.initial_headword_keys)))))")
            .bind(id).bind(annotation_id).bind(input.entry_id).execute(&mut *tx).await.map_err(AppError::internal)?.rows_affected();
        if changed != 1 {
            return Err(AppError::conflict(
                ErrorCode::PendingSentenceAssociationClaimed,
                None,
                "待关联标记已变化或与此词条不匹配",
            ));
        }
        eligible = true;
    }
    if !eligible {
        return Err(invalid("此例句未标注当前词条，请先确认关联"));
    }
    sqlx::query("INSERT INTO lexicon.shared_sentence_collections(sentence_id,entry_id) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(id).bind(input.entry_id).execute(&mut *tx).await.map_err(AppError::internal)?;
    bump(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(read(&state.pool, id).await?))
}

#[utoipa::path(delete,path="/api/v1/admin/lexicon/sentences/{id}/collections/{entry_id}",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path),("entry_id"=Uuid,Path)),request_body=SentenceRevision,responses((status=204,description="删除或移除收录成功"),(status=400,description="输入无效"),(status=401,description="未登录"),(status=403,description="管理员不可用或无权编辑目标词条"),(status=404,description="例句或词条不存在"),(status=409,description="版本冲突"),(status=422,description="请求结构无效")))]
pub async fn uncollect(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath((id, entry_id)): ApiPath<(Uuid, Uuid)>,
    ApiJson(input): ApiJson<SentenceRevision>,
) -> Result<StatusCode, AppError> {
    let admin = require_active_admin(&state, &auth).await?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock_targets(&mut tx, None, Some(entry_id)).await?;
    writable_entry(&mut tx, entry_id, &admin).await?;
    lock(&mut tx, id, input.base_revision).await?;
    sqlx::query(
        "DELETE FROM lexicon.shared_sentence_collections WHERE sentence_id=$1 AND entry_id=$2",
    )
    .bind(id)
    .bind(entry_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    bump(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}
