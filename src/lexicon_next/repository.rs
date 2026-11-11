use super::{language, model::*};
use crate::{
    admin::{AdminAuth, permissions},
    error::{AppError, ErrorCode},
};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct EntryRow {
    id: Uuid,
    target_language: String,
    kind: String,
    owner_id: Uuid,
    revision: i64,
    updated_at: chrono::DateTime<chrono::Utc>,
    document: serde_json::Value,
}
impl EntryRow {
    fn entry(self) -> Result<NextEntry, AppError> {
        Ok(NextEntry {
            id: self.id,
            target_language: self.target_language,
            kind: serde_json::from_value(serde_json::Value::String(self.kind))
                .map_err(AppError::internal)?,
            owner_id: self.owner_id,
            revision: self.revision,
            updated_at: self.updated_at,
            draft: serde_json::from_value(self.document).map_err(AppError::internal)?,
        })
    }
}
const SELECT_ENTRY: &str = "SELECT e.id,e.target_language,e.kind,e.owner_id,e.revision,e.updated_at,r.document FROM lexicon_next.entries e JOIN lexicon_next.draft_revisions r ON r.entry_id=e.id AND r.revision=e.revision WHERE e.id=$1";

pub async fn list(pool: &PgPool, query: NextListQuery) -> Result<NextEntryList, AppError> {
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);
    if page < 1 || !(1..=100).contains(&page_size) {
        return Err(language::invalid());
    }
    let offset = i64::from(page - 1) * i64::from(page_size);
    let headword = query.headword.unwrap_or_default().trim().to_owned();
    if headword.chars().count() > 200 {
        return Err(language::invalid());
    }
    let kind = query.kind.map(|kind| match kind {
        NextEntryKind::Word => "word",
        NextEntryKind::Phrase => "phrase",
    });
    // Count and page share one snapshot, including concurrent creates/saves.
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let total = sqlx::query_scalar("SELECT count(*) FROM lexicon_next.entries e WHERE position(lower($1) in lower(e.headword)) > 0 AND ($2::text IS NULL OR e.kind=$2)")
        .bind(&headword).bind(kind).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    let items = sqlx::query_as::<_, NextEntrySummary>(r#"
        SELECT e.id,e.target_language,e.kind,e.headword,e.owner_id,e.revision,e.updated_at,
            ARRAY(SELECT DISTINCT s.part_of_speech FROM lexicon_next.senses s
                JOIN lexicon_next.nodes n ON n.id=s.id
                WHERE n.entry_id=e.id AND s.part_of_speech IS NOT NULL ORDER BY s.part_of_speech) AS part_of_speech,
            (SELECT d.text FROM lexicon_next.definitions d JOIN lexicon_next.nodes n ON n.id=d.id
                WHERE n.entry_id=e.id ORDER BY CASE d.locale WHEN 'zh-Hans' THEN 0 WHEN 'zh-Hant' THEN 1 ELSE 2 END,d.sort_order,d.id LIMIT 1) AS gloss
        FROM lexicon_next.entries e
        WHERE position(lower($1) in lower(e.headword)) > 0 AND ($2::text IS NULL OR e.kind=$2)
        ORDER BY e.updated_at DESC,e.id DESC LIMIT $3 OFFSET $4
    "#).bind(&headword).bind(kind).bind(i64::from(page_size)).bind(offset)
        .fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(NextEntryList {
        items,
        pagination: crate::api::page_meta(page, page_size, total),
    })
}
pub async fn get(pool: &PgPool, id: Uuid) -> Result<NextEntry, AppError> {
    sqlx::query_as::<_, EntryRow>(SELECT_ENTRY)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("lexicon-next entry not found"))?
        .entry()
}
pub async fn create(
    pool: &PgPool,
    auth: &AdminAuth,
    input: CreateNextEntry,
) -> Result<NextEntry, AppError> {
    language::require_language(&input.target_language)?;
    language::text(&input.headword, 200)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, auth, "words.create").await?;
    let id = Uuid::now_v7();
    let kind = match input.kind {
        NextEntryKind::Word => "word",
        NextEntryKind::Phrase => "phrase",
    };
    sqlx::query("INSERT INTO lexicon_next.entries(id,target_language,kind,headword,owner_id,revision) VALUES($1,$2,$3,$4,$5,1)")
        .bind(id).bind(&input.target_language).bind(kind).bind(&input.headword).bind(auth.subject).execute(&mut *tx).await.map_err(AppError::internal)?;
    let draft = NextDraft {
        schema_version: 1,
        headword: input.headword,
        forms: vec![],
        senses: vec![],
    };
    snapshot(&mut tx, id, 1, auth.subject, &draft).await?;
    let result = sqlx::query_as::<_, EntryRow>(SELECT_ENTRY)
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?
        .entry()?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
async fn snapshot(
    conn: &mut PgConnection,
    id: Uuid,
    revision: i64,
    actor: Uuid,
    draft: &NextDraft,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO lexicon_next.draft_revisions(entry_id,revision,actor_id,document) VALUES($1,$2,$3,$4)")
        .bind(id).bind(revision).bind(actor).bind(serde_json::to_value(draft).map_err(AppError::internal)?)
        .execute(conn).await.map_err(AppError::internal)?;
    Ok(())
}
async fn node(
    conn: &mut PgConnection,
    id: Uuid,
    entry: Uuid,
    kind: &str,
    parent: Option<Uuid>,
) -> Result<(), AppError> {
    // ON CONFLICT waits for a concurrent claimant; compare the permanent identity afterwards.
    sqlx::query("INSERT INTO lexicon_next.nodes(id,entry_id,kind,parent_id) VALUES($1,$2,$3,$4) ON CONFLICT(id) DO NOTHING")
        .bind(id).bind(entry).bind(kind).bind(parent).execute(&mut *conn).await.map_err(AppError::internal)?;
    let existing: (Uuid, String, Option<Uuid>) =
        sqlx::query_as("SELECT entry_id,kind,parent_id FROM lexicon_next.nodes WHERE id=$1")
            .bind(id)
            .fetch_one(conn)
            .await
            .map_err(AppError::internal)?;
    if existing != (entry, kind.to_owned(), parent) {
        return Err(AppError::conflict(
            ErrorCode::StableNodeIdChanged,
            None,
            "node identity belongs to another entry, kind or sense",
        ));
    }
    Ok(())
}
pub async fn save(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    input: SaveNextDraft,
) -> Result<NextEntry, AppError> {
    if input.expected_revision < 1 {
        return Err(language::invalid());
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, auth, "words.edit").await?;
    let (language, owner, revision): (String, Uuid, i64) = sqlx::query_as(
        "SELECT target_language,owner_id,revision FROM lexicon_next.entries WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("lexicon-next entry not found"))?;
    permissions::lock(&mut tx, auth.subject)
        .await?
        .require_owned_action("words.edit", owner)?;
    if revision != input.expected_revision {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            Some("expected_revision"),
            "draft changed; reload before saving",
        ));
    }
    let conn = &mut *tx;
    let draft = &input.draft;
    let actor = auth.subject;
    language::validate(&language, draft)?;
    // Only current content is replaced. Nodes and previous revision snapshots remain intact.
    for sql in [
        "DELETE FROM lexicon_next.equivalents WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=$1)",
        "DELETE FROM lexicon_next.definitions WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=$1)",
        "DELETE FROM lexicon_next.senses WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=$1)",
        "DELETE FROM lexicon_next.forms WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=$1)",
    ] {
        sqlx::query(sql)
            .bind(id)
            .execute(&mut *conn)
            .await
            .map_err(AppError::internal)?;
    }
    for (order, form) in draft.forms.iter().enumerate() {
        node(conn, form.id, id, "form", None).await?;
        sqlx::query(
            "INSERT INTO lexicon_next.forms(id,locale,text,sort_order) VALUES($1,$2,$3,$4)",
        )
        .bind(form.id)
        .bind(&form.locale)
        .bind(&form.text)
        .bind(order as i32)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    }
    for (order, sense) in draft.senses.iter().enumerate() {
        node(conn, sense.id, id, "sense", None).await?;
        sqlx::query(
            "INSERT INTO lexicon_next.senses(id,part_of_speech,sort_order) VALUES($1,$2,$3)",
        )
        .bind(sense.id)
        .bind(&sense.part_of_speech)
        .bind(order as i32)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
        for (order, definition) in sense.definitions.iter().enumerate() {
            node(conn, definition.id, id, "definition", Some(sense.id)).await?;
            let style = match definition.style {
                NextDefinitionStyle::Definition => "definition",
                NextDefinitionStyle::Sentence => "sentence",
            };
            sqlx::query("INSERT INTO lexicon_next.definitions(id,sense_id,locale,text,style,sort_order) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(definition.id).bind(sense.id).bind(&definition.locale).bind(&definition.text).bind(style).bind(order as i32).execute(&mut *conn).await.map_err(AppError::internal)?;
        }
        for (order, equivalent) in sense.equivalents.iter().enumerate() {
            node(conn, equivalent.id, id, "equivalent", Some(sense.id)).await?;
            sqlx::query("INSERT INTO lexicon_next.equivalents(id,sense_id,locale,text,sort_order) VALUES($1,$2,$3,$4,$5)")
                .bind(equivalent.id).bind(sense.id).bind(&equivalent.locale).bind(&equivalent.text).bind(order as i32).execute(&mut *conn).await.map_err(AppError::internal)?;
        }
    }
    let next_revision = revision.checked_add(1).ok_or_else(language::invalid)?;
    snapshot(conn, id, next_revision, actor, draft).await?;
    sqlx::query("UPDATE lexicon_next.entries SET headword=$2,revision=$3,updated_at=clock_timestamp() WHERE id=$1")
        .bind(id).bind(&draft.headword).bind(next_revision).execute(&mut *conn).await.map_err(AppError::internal)?;
    let result = sqlx::query_as::<_, EntryRow>(SELECT_ENTRY)
        .bind(id)
        .fetch_one(&mut *conn)
        .await
        .map_err(AppError::internal)?
        .entry()?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
