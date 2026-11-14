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
    medium_locale: String,
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
            medium_locale: Some(self.medium_locale),
            kind: serde_json::from_value(serde_json::Value::String(self.kind))
                .map_err(AppError::internal)?,
            owner_id: self.owner_id,
            revision: self.revision,
            updated_at: self.updated_at,
            draft: serde_json::from_value(self.document).map_err(AppError::internal)?,
        })
    }
}
const SELECT_ENTRY: &str = "SELECT e.id,e.target_language,e.medium_locale,e.kind,e.owner_id,e.revision,e.updated_at,r.document FROM lexicon_next.entries e JOIN lexicon_next.draft_revisions r ON r.entry_id=e.id AND r.revision=e.revision WHERE e.id=$1";

pub(super) async fn lock_list_writers(conn: &mut PgConnection) -> Result<(), AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('lexicon_next.list-writer',0))")
        .execute(conn)
        .await
        .map_err(AppError::internal)?;
    Ok(())
}

pub async fn list(pool: &PgPool, query: NextListQuery) -> Result<NextEntryList, AppError> {
    let query = super::list_model::NextWorkspaceQuery {
        target_language: query.target_language,
        medium_locale: query.medium_locale,
        headword: query.headword,
        kind: query.kind.map(|kind| match kind {
            NextEntryKind::Word => "word".into(),
            NextEntryKind::Phrase => "phrase".into(),
        }),
        page: query.page,
        page_size: query.page_size,
        ..Default::default()
    };
    super::workspace::validate(&query)?;
    let page = query.page.unwrap_or(1);
    let size = query.page_size.unwrap_or(20);
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let mut count = sqlx::QueryBuilder::new("SELECT count(*) FROM lexicon_next.entries e");
    super::workspace::filter(&mut count, &query);
    let total = count
        .build_query_scalar()
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let mut rows = super::workspace::select(query.medium_locale.as_deref());
    super::workspace::filter(&mut rows, &query);
    rows.push(" ORDER BY e.updated_at DESC,e.id DESC LIMIT ")
        .push_bind(i64::from(size))
        .push(" OFFSET ")
        .push_bind(i64::from(page - 1) * i64::from(size));
    let items = rows
        .build_query_as()
        .fetch_all(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(NextEntryList {
        items,
        pagination: crate::api::page_meta(page, size, total),
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
    language::require_medium(&input.medium_locale)?;
    language::text(&input.headword, 200)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, auth, "words.create").await?;
    lock_list_writers(&mut tx).await?;
    let id = Uuid::now_v7();
    let kind = match input.kind {
        NextEntryKind::Word => "word",
        NextEntryKind::Phrase => "phrase",
    };
    sqlx::query("INSERT INTO lexicon_next.entries(id,target_language,medium_locale,kind,headword,owner_id,revision) VALUES($1,$2,$3,$4,$5,$6,1)")
        .bind(id).bind(&input.target_language).bind(&input.medium_locale).bind(kind).bind(&input.headword).bind(auth.subject).execute(&mut *tx).await.map_err(AppError::internal)?;
    let draft = NextDraft {
        sense_groups: None,
        schema_version: 1,
        headword: input.headword,
        forms: vec![],
        senses: vec![],
        form_editor: None,
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
    mut input: SaveNextDraft,
) -> Result<NextEntry, AppError> {
    if input.expected_revision < 1 {
        return Err(language::invalid());
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, auth, "words.edit").await?;
    lock_list_writers(&mut tx).await?;
    let (language, owner, revision, status): (String, Uuid, i64, String) = sqlx::query_as(
        "SELECT target_language,owner_id,revision,status FROM lexicon_next.entries WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("lexicon-next entry not found"))?;
    permissions::lock(&mut tx, auth.subject)
        .await?
        .require_owned_action("words.edit", owner)?;
    if status == "archived" {
        return Err(AppError::conflict(
            ErrorCode::EntryArchived,
            None,
            "归档词条不能编辑，请先恢复",
        ));
    }
    if revision != input.expected_revision {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            Some("expected_revision"),
            "draft changed; reload before saving",
        ));
    }
    let conn = &mut *tx;
    let draft = &mut input.draft;
    let actor = auth.subject;
    language::validate(&language, draft)?;
    let previous = sqlx::query_as::<_, EntryRow>(SELECT_ENTRY)
        .bind(id)
        .fetch_one(&mut *conn)
        .await
        .map_err(AppError::internal)?
        .entry()?;
    if previous.draft.sense_groups.is_some() && draft.sense_groups.is_none() {
        return Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "sense groups require an updated client; reopen the entry before saving",
        ));
    }
    if previous.draft.form_editor.is_some() && draft.form_editor.is_none() {
        return Err(crate::lexicon::shared_forms::upgrade_error());
    }
    if let Some(editor) = &draft.form_editor {
        let forms = editor
            .clone()
            .resolve()
            .map_err(|error| error.into_app_error())?;
        let kind = match previous.kind {
            NextEntryKind::Word => "word",
            NextEntryKind::Phrase => "phrase",
        };
        crate::lexicon::service::v3::resolve_v3_catalog_parts(&mut tx, &forms, kind)
            .await
            .map_err(crate::lexicon::handler::map_error)?;
    }
    let conn = &mut *tx;
    super::forms::persist(conn, auth.subject, id, draft).await?;
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
    for group in draft.sense_groups.as_deref().unwrap_or_default() {
        node(conn, group.id, id, "sense_group", None).await?;
    }
    for (order, form) in super::forms::projection(draft)?.iter().enumerate() {
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
