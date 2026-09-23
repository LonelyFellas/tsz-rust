use std::collections::HashMap;

use super::publication::SentencePublication;
use super::*;

pub(super) async fn summaries(
    conn: &mut PgConnection,
    content: &SharedSentenceContent,
) -> Result<Vec<SharedSentenceEntry>, AppError> {
    let targets=content.annotations.iter().filter_map(|a|match &a.target {
        SentenceTarget::Linked {target_entry_id,target_sense_id,target_publication_id,..} => Some(serde_json::json!({"entry_id":target_entry_id,"sense_id":target_sense_id,"publication_id":target_publication_id})),
        SentenceTarget::EntryOnly {target_entry_id} => Some(serde_json::json!({"entry_id":target_entry_id,"sense_id":null,"publication_id":null})),
        _=>None,
    }).collect::<Vec<_>>();
    let rows=sqlx::query("SELECT DISTINCT e.id,e.kind,COALESCE(CASE WHEN t.publication_id IS NOT NULL THEN pub.snapshot->'presentation'->>'label' ELSE p.label END,'') AS headword,t.sense_id,COALESCE(pub.snapshot->'meanings',d.meanings) AS meanings FROM jsonb_to_recordset($1::jsonb) AS t(entry_id uuid,sense_id uuid,publication_id uuid) JOIN lexicon.entries e ON e.id=t.entry_id LEFT JOIN lexicon.entry_presentation_projection p ON p.entry_id=e.id LEFT JOIN lexicon.entry_editor_projection d ON d.entry_id=e.id LEFT JOIN lexicon.entry_publications pub ON pub.id=t.publication_id AND pub.entry_id=e.id ORDER BY e.id,t.sense_id")
        .bind(serde_json::json!(targets)).fetch_all(conn).await.map_err(AppError::internal)?;
    let mut entries = HashMap::<Uuid, SharedSentenceEntry>::new();
    for row in rows {
        let entry = entries
            .entry(row.get("id"))
            .or_insert_with(|| SharedSentenceEntry {
                id: row.get("id"),
                headword: row.get("headword"),
                kind: row.get("kind"),
                senses: vec![],
            });
        let Some(sense_id) = row.get::<Option<Uuid>, _>("sense_id") else {
            continue;
        };
        if entry.senses.iter().any(|s| s.id == sense_id) {
            continue;
        }
        let meanings: Option<DraftMeaningsStepContentV3> = row
            .get::<Option<serde_json::Value>, _>("meanings")
            .map(serde_json::from_value)
            .transpose()
            .map_err(AppError::internal)?;
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
        entry.senses.push(SentenceSenseSummary {
            id: sense_id,
            gloss,
        });
    }
    let mut entries = entries.into_values().collect::<Vec<_>>();
    entries.sort_by(|a, b| (&a.headword, a.id).cmp(&(&b.headword, b.id)));
    Ok(entries)
}

pub(super) async fn published_on(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<SharedSentence, AppError> {
    let row=sqlx::query("SELECT s.*,a.display_name AS created_by,p.snapshot FROM lexicon.shared_sentences s JOIN admins a ON a.id=s.created_by_admin_id JOIN lexicon.shared_sentence_publications p ON p.id=s.current_publication_id WHERE s.id=$1 AND s.deleted_at IS NULL AND s.withdrawn_at IS NULL")
        .bind(id).fetch_optional(&mut *conn).await.map_err(AppError::internal)?.ok_or_else(missing)?;
    let content = serde_json::from_value(row.get("snapshot")).map_err(AppError::internal)?;
    let entries = summaries(conn, &content).await?;
    Ok(SharedSentence {
        id,
        revision: row.get("revision"),
        lifecycle_revision: row.get("lifecycle_revision"),
        current_publication_id: row.get("current_publication_id"),
        withdrawn_at: row.get("withdrawn_at"),
        withdrawn_reason: row.get("withdrawn_reason"),
        view: SentenceView::Published,
        content,
        entries,
        created_by: row.get("created_by"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

pub(super) async fn published_list(
    pool: &PgPool,
    q: &SentenceListQuery,
) -> Result<SharedSentenceList, AppError> {
    if q.pending_entry_id.is_some() {
        return Err(AppError::validation(
            ErrorCode::InvalidQuery,
            "view",
            "待关联修复搜索须显式使用 draft 视图",
        ));
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let association = match q.association_status {
        Some(SentenceAssociationStatus::Pending) => Some("pending"),
        Some(SentenceAssociationStatus::EntryOnly) => Some("entry_only"),
        Some(SentenceAssociationStatus::Unlinked) => Some("unlinked"),
        None => None,
    };
    macro_rules! published_filter { () => { r#"FROM lexicon.shared_sentences s
        JOIN lexicon.shared_sentence_publications p ON p.id=s.current_publication_id
        JOIN admins creator ON creator.id=s.created_by_admin_id
        WHERE s.deleted_at IS NULL AND s.withdrawn_at IS NULL
        AND ($1::text IS NULL OR s.id::text ILIKE '%'||$1||'%' OR p.snapshot::text ILIKE '%'||$1||'%' OR creator.display_name ILIKE '%'||$1||'%')
        AND ($2::text IS NULL OR p.snapshot->'sentence'->>'level'=$2)
        AND ($3::timestamptz IS NULL OR s.created_at >= $3) AND ($4::timestamptz IS NULL OR s.created_at <= $4)
        AND ($5::uuid IS NULL OR EXISTS(
            SELECT 1 FROM lexicon.shared_sentence_publication_annotations a JOIN lexicon.entries e ON e.id=a.target_entry_id
            WHERE a.publication_id=p.id AND a.target_entry_id=$5 AND ($6::uuid IS NULL OR a.target_sense_id=$6)
              AND e.current_publication_id IS NOT NULL AND e.archived_at IS NULL
              AND NOT EXISTS(SELECT 1 FROM lexicon.shared_sentence_publication_hides h WHERE h.publication_id=e.current_publication_id AND h.sense_id=a.target_sense_id AND h.sentence_id=s.id)))
        AND ($7::text IS NULL
          OR ($7='pending' AND EXISTS(SELECT 1 FROM lexicon.shared_sentence_publication_annotations a WHERE a.publication_id=p.id AND a.pending_kind IS NOT NULL))
          OR ($7='unlinked' AND NOT EXISTS(SELECT 1 FROM lexicon.shared_sentence_publication_annotations a WHERE a.publication_id=p.id AND a.target_entry_id IS NOT NULL)))"# }; }
    let total = sqlx::query_scalar::<_, i64>(concat!("SELECT count(*) ", published_filter!()))
        .bind(&q.q)
        .bind(&q.level)
        .bind(q.created_from)
        .bind(q.created_to)
        .bind(q.entry_id)
        .bind(q.sense_id)
        .bind(association)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let ids=sqlx::query_scalar::<_,Uuid>(concat!("SELECT s.id ", published_filter!(), " ORDER BY CASE WHEN $8 THEN s.updated_at ELSE s.created_at END DESC,s.id LIMIT $9 OFFSET $10"))
        .bind(&q.q).bind(&q.level).bind(q.created_from).bind(q.created_to).bind(q.entry_id).bind(q.sense_id).bind(association)
        .bind(matches!(q.sort,Some(SentenceListSort::UpdatedAtDesc))).bind(q.page_size.unwrap_or(10)).bind((q.page.unwrap_or(1)-1)*q.page_size.unwrap_or(10))
        .fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        items.push(published_on(&mut tx, id).await?);
    }
    Ok(SharedSentenceList { items, total })
}

fn publication_from_row(row: sqlx::postgres::PgRow) -> Result<SentencePublication, AppError> {
    Ok(SentencePublication {
        id: row.get("id"),
        sentence_id: row.get("sentence_id"),
        publication_number: row.get("publication_number"),
        source_revision: row.get("source_revision"),
        snapshot: serde_json::from_value(row.get("snapshot")).map_err(AppError::internal)?,
        published_at: row.get("published_at"),
        published_by_admin_id: row.get("published_by_admin_id"),
        rollback_of_publication_id: row.get("rollback_of_publication_id"),
    })
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in=Query)]
pub struct SentenceHistoryQuery {
    pub before_number: Option<i64>,
}

#[utoipa::path(get,path="/api/v1/admin/lexicon/sentences/{id}/publications",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path),SentenceHistoryQuery),responses((status=200,body=Vec<SentencePublication>)))]
pub async fn history(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(q): ApiQuery<SentenceHistoryQuery>,
) -> Result<Json<Vec<SentencePublication>>, AppError> {
    require_active_admin(&state, &auth).await?;
    if q.before_number.is_some_and(|n| n <= 0) {
        return Err(AppError::validation(
            ErrorCode::InvalidQuery,
            "before_number",
            "发布序号必须为正数",
        ));
    }
    let rows=sqlx::query("SELECT * FROM lexicon.shared_sentence_publications WHERE sentence_id=$1 AND ($2::bigint IS NULL OR publication_number<$2) ORDER BY publication_number DESC LIMIT 50")
        .bind(id).bind(q.before_number).fetch_all(&state.pool).await.map_err(AppError::internal)?;
    Ok(Json(
        rows.into_iter()
            .map(publication_from_row)
            .collect::<Result<_, _>>()?,
    ))
}

#[utoipa::path(get,path="/api/v1/admin/lexicon/sentences/{id}/publications/{publication_id}",tag="admin-lexicon",security(("bearer_auth"=[])),params(("id"=Uuid,Path),("publication_id"=Uuid,Path)),responses((status=200,body=SentencePublication),(status=404,description="发布记录不存在")))]
pub async fn historical(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath((id, publication_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<Json<SentencePublication>, AppError> {
    require_active_admin(&state, &auth).await?;
    let row = sqlx::query(
        "SELECT * FROM lexicon.shared_sentence_publications WHERE sentence_id=$1 AND id=$2",
    )
    .bind(id)
    .bind(publication_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(missing)?;
    Ok(Json(publication_from_row(row)?))
}
