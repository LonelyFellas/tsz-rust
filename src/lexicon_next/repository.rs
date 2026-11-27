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
    creation_detection: Option<serde_json::Value>,
    document: serde_json::Value,
}
impl EntryRow {
    fn entry(self) -> Result<NextEntry, AppError> {
        let mut document = self.document;
        let legacy_detection = document
            .as_object_mut()
            .and_then(|object| object.remove("creation_detection"))
            .map(serde_json::from_value)
            .transpose()
            .map_err(AppError::internal)?;
        super::model::strip_legacy_creation_detection(&mut document);
        Ok(NextEntry {
            dialect_senses: None,
            sentence_resources: None,
            creation_detection: self
                .creation_detection
                .map(serde_json::from_value)
                .transpose()
                .map_err(AppError::internal)?
                .or(legacy_detection),
            id: self.id,
            target_language: self.target_language,
            medium_locale: Some(self.medium_locale),
            kind: serde_json::from_value(serde_json::Value::String(self.kind))
                .map_err(AppError::internal)?,
            owner_id: self.owner_id,
            revision: self.revision,
            updated_at: self.updated_at,
            draft: serde_json::from_value(document).map_err(AppError::internal)?,
        })
    }
}
const SELECT_ENTRY: &str = "SELECT e.id,e.target_language,e.medium_locale,e.kind,e.owner_id,e.revision,e.updated_at,e.creation_detection,r.document FROM lexicon_next.entries e JOIN lexicon_next.draft_revisions r ON r.entry_id=e.id AND r.revision=e.revision WHERE e.id=$1";

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
    let mut conn = pool.acquire().await.map_err(AppError::internal)?;
    let mut entry = sqlx::query_as::<_, EntryRow>(SELECT_ENTRY)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("lexicon-next entry not found"))?
        .entry()?;
    super::grammar::hydrate(&mut conn, id, &mut entry.draft).await?;
    Ok(entry)
}
pub(super) async fn get_on(conn: &mut PgConnection, id: Uuid) -> Result<NextEntry, AppError> {
    let mut entry = sqlx::query_as::<_, EntryRow>(SELECT_ENTRY)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("lexicon-next entry not found"))?
        .entry()?;
    super::grammar::hydrate(conn, id, &mut entry.draft).await?;
    Ok(entry)
}
pub async fn create(
    pool: &PgPool,
    auth: &AdminAuth,
    mut input: CreateNextEntry,
    test_run: Option<&str>,
) -> Result<NextEntry, AppError> {
    language::require_language(&input.target_language)?;
    language::require_medium(&input.medium_locale)?;
    language::text(&input.headword, 200)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, auth, "words.create").await?;
    lock_list_writers(&mut tx).await?;
    let id = Uuid::now_v7();
    let creation_detection = input
        .creation_detection
        .take()
        .map(serde_json::to_value)
        .transpose()
        .map_err(AppError::internal)?;
    let kind = match input.kind {
        NextEntryKind::Word => "word",
        NextEntryKind::Phrase => "phrase",
    };
    sqlx::query("INSERT INTO lexicon_next.entries(id,target_language,medium_locale,kind,headword,owner_id,revision,creation_detection) VALUES($1,$2,$3,$4,$5,$6,1,$7)")
        .bind(id).bind(&input.target_language).bind(&input.medium_locale).bind(kind).bind(&input.headword).bind(auth.subject).bind(creation_detection).execute(&mut *tx).await.map_err(AppError::internal)?;
    if let Some(run) = test_run {
        crate::test_resources::lock_run(&mut tx, auth.subject, run).await?;
        crate::test_resources::register_entry(&mut tx, run, auth.subject, id).await?;
    }
    let draft = NextDraft {
        grammar_structures: None,
        sense_groups: None,
        schema_version: 1,
        headword: input.headword,
        forms: vec![],
        senses: vec![],
        form_editor: None,
        form_question_eligibility: Default::default(),
    };
    snapshot(&mut tx, id, 1, auth.subject, &draft, &[]).await?;
    let result = sqlx::query_as::<_, EntryRow>(SELECT_ENTRY)
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?
        .entry()?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub(super) async fn snapshot(
    conn: &mut PgConnection,
    id: Uuid,
    revision: i64,
    actor: Uuid,
    draft: &NextDraft,
    dialects: &[super::dialect_pairs::Binding],
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO lexicon_next.draft_revisions(entry_id,revision,actor_id,document,dialect_senses) VALUES($1,$2,$3,$4,$5)")
        .bind(id).bind(revision).bind(actor).bind(serde_json::to_value(draft).map_err(AppError::internal)?).bind(sqlx::types::Json(dialects))
        .execute(conn).await.map_err(AppError::internal)?;
    Ok(())
}
pub(super) async fn node(
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
pub(super) struct SaveCapabilities {
    pub sentence_drafts: bool,
    pub relations: bool,
    pub pairs: bool,
    pub grammar_audio: bool,
    pub grammar_form_links: bool,
    pub sense_metadata: bool,
    pub equivalent_metadata: bool,
    pub definition_details: bool,
    pub sentences: bool,
}

pub async fn save(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    mut input: SaveNextDraft,
    run: Option<&str>,
    capabilities: SaveCapabilities,
) -> Result<NextEntry, AppError> {
    if !input.sentence_changes.is_empty() && !capabilities.sentence_drafts {
        return Err(language::unavailable());
    }
    if input.expected_revision < 1 {
        return Err(language::invalid());
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    crate::lexicon::rich_text_repair::lock_transaction(&mut tx)
        .await
        .map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, auth, "words.edit").await?;
    lock_list_writers(&mut tx).await?;
    if let Some(run) = run {
        crate::test_resources::lock_run(&mut tx, auth.subject, run).await?;
    }
    let (language, owner, revision, status): (String, Uuid, i64, String) = sqlx::query_as(
        "SELECT target_language,owner_id,revision,status FROM lexicon_next.entries WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("lexicon-next entry not found"))?;
    let access = permissions::lock(&mut tx, auth.subject).await?;
    access.require_owned_action("words.edit", owner)?;
    let registered = crate::test_resources::entry_run(&mut tx, id).await?;
    crate::test_resources::require_same_run(run, registered.as_deref())?;
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
    if let Some(editor) = draft.form_editor.as_mut() {
        crate::lexicon::rich_text_repair::repair_shared_forms(editor);
    }
    language::validate(&language, draft)?;
    let previous = sqlx::query_as::<_, EntryRow>(SELECT_ENTRY)
        .bind(id)
        .fetch_one(&mut *conn)
        .await
        .map_err(AppError::internal)?
        .entry()?;
    super::forms::prepare_question_eligibility(conn, draft, &previous.draft).await?;
    if !capabilities.grammar_form_links {
        super::grammar::preserve_form_links(draft, &previous.draft);
    }
    if !capabilities.definition_details {
        super::definitions::preserve(draft, &previous.draft);
    }
    super::definitions::validate(draft)?;
    super::sentences::preserve(draft, &previous.draft, capabilities.sentences)?;
    super::relations::prepare(conn, id, draft, &previous, capabilities.relations).await?;
    language::validate(&language, draft)?;
    let taxonomy_refs = super::equivalents::prepare(
        conn,
        draft,
        &previous.draft,
        capabilities.equivalent_metadata,
    )
    .await?;
    if !capabilities.sense_metadata {
        for sense in &mut draft.senses {
            if let Some(saved) = previous
                .draft
                .senses
                .iter()
                .find(|saved| saved.id == sense.id)
            {
                sense.level = saved.level.clone();
                sense.frequency = saved.frequency.clone();
                sense.depends_on_context = saved.depends_on_context;
            }
        }
    }
    if previous.draft.grammar_structures.is_some() && draft.grammar_structures.is_none() {
        return Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "grammar structures require an updated client; reopen before saving",
        ));
    }
    if !capabilities.grammar_audio {
        super::grammar::preserve_audio(draft, &previous.draft);
    }
    super::grammar::validate_catalog(
        &mut tx,
        draft,
        match previous.kind {
            NextEntryKind::Word => "word",
            NextEntryKind::Phrase => "phrase",
        },
    )
    .await?;
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
    super::sentence_drafts::save_changes(
        &mut tx,
        auth,
        &previous,
        draft,
        input.sentence_changes,
        run,
    )
    .await?;
    let conn = &mut *tx;
    super::forms::persist(conn, id, draft).await?;
    super::grammar::persist(conn, id, draft).await?;
    super::audio::persist(conn, auth.subject, id, draft).await?;
    super::definitions::validate_bindings(conn, draft).await?;
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
        for relation in sense.relations.iter().flatten() {
            node(conn, relation.id, id, "relation", Some(sense.id)).await?;
        }
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
            sqlx::query("INSERT INTO lexicon_next.equivalents(id,sense_id,locale,text,sort_order,metadata) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(equivalent.id).bind(sense.id).bind(&equivalent.locale).bind(&equivalent.text).bind(order as i32).bind(equivalent.metadata.as_ref().map(serde_json::to_value).transpose().map_err(AppError::internal)?).execute(&mut *conn).await.map_err(AppError::internal)?;
        }
    }
    let next_revision = revision.checked_add(1).ok_or_else(language::invalid)?;
    super::sentences::persist_references(&mut tx, actor, id, &language, draft, &previous.draft)
        .await?;
    let conn = &mut *tx;
    let dialects =
        super::dialect_pairs::apply(conn, &access, &previous, draft, input.dialect_changes).await?;
    snapshot(conn, id, next_revision, actor, draft, &dialects).await?;
    super::equivalents::persist_refs(conn, id, next_revision, taxonomy_refs).await?;
    let legacy_detection = previous
        .creation_detection
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(AppError::internal)?;
    sqlx::query("UPDATE lexicon_next.entries SET headword=$2,revision=$3,updated_at=clock_timestamp(),creation_detection=COALESCE(creation_detection,$4) WHERE id=$1")
        .bind(id).bind(&draft.headword).bind(next_revision).bind(legacy_detection).execute(&mut *conn).await.map_err(AppError::internal)?;
    let mut result = sqlx::query_as::<_, EntryRow>(SELECT_ENTRY)
        .bind(id)
        .fetch_one(&mut *conn)
        .await
        .map_err(AppError::internal)?
        .entry()?;
    super::grammar::hydrate(conn, id, &mut result.draft).await?;
    if capabilities.pairs {
        result.dialect_senses =
            Some(super::dialect_pairs::project(conn, id, next_revision, false).await?);
    }
    if capabilities.sentence_drafts
        && permissions::lock(&mut tx, actor)
            .await?
            .has("sentences.access")
    {
        result.sentence_resources = Some(super::sentence_drafts::resources(&mut tx, draft).await?);
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
