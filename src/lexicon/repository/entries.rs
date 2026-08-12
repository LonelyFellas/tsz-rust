use super::*;

// --- helpers ---

pub(super) fn map_entry_write_error(error: sqlx::Error) -> LexiconRepositoryError {
    if is_unique_violation(&error, "lexicon_entry_headword_keys_unique_idx") {
        return LexiconRepositoryError::DuplicateHeadword;
    }
    LexiconRepositoryError::Database(error)
}

pub(super) fn map_target_publication_lock_error(error: sqlx::Error) -> LexiconRepositoryError {
    if matches!(&error, sqlx::Error::Database(database)
        if database.code().as_deref() == Some("55P03"))
    {
        LexiconRepositoryError::TargetPublicationBusy
    } else {
        LexiconRepositoryError::Database(error)
    }
}

pub(super) fn escape_like_literal(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub(super) fn kind_string(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Word => "word",
        EntryKind::Phrase => "phrase",
    }
}

pub(super) fn dialect_string(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Common => "common",
        Dialect::Uk => "uk",
        Dialect::Us => "us",
    }
}

pub(super) fn source_dialect_string(dialect: crate::lexicon::dto::SourceDialect) -> &'static str {
    match dialect {
        crate::lexicon::dto::SourceDialect::Uk => "uk",
        crate::lexicon::dto::SourceDialect::Us => "us",
    }
}

pub(super) fn origin_string(origin: TextOrigin) -> &'static str {
    match origin {
        TextOrigin::Dictionary => "dictionary",
        TextOrigin::Converted => "converted",
        TextOrigin::Manual => "manual",
    }
}

// --- entry commands ---

impl LexiconRepository {
    pub(crate) async fn insert_entry(
        tx: &mut Transaction<'_, Postgres>,
        word: &AdminWordV2,
        actor_id: Uuid,
        request_id: Uuid,
        catalog_parts: &HashMap<String, Uuid>,
        idempotency_key: Uuid,
        request_hash: &[u8],
    ) -> Result<(), LexiconRepositoryError> {
        let (headword_mode, source_dialect) = match &word.headwords {
            WordHeadwordsV2::Unified { .. } => ("unified", None),
            WordHeadwordsV2::Distinguish { source_dialect, .. } => {
                ("distinguish", Some(source_dialect_string(*source_dialect)))
            }
        };
        let detection_snapshot = serde_json::to_value(&word.detection_snapshot)?;

        sqlx::query(
            r#"
            INSERT INTO lexicon.entries (
                id, content_schema_version, language, kind, revision,
                headword_mode, source_dialect, frequency, detection_snapshot,
                created_by_admin_id, updated_by_admin_id, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::numeric, $9, $10, $10, $11, $11)
            "#,
        )
        .bind(word.id)
        .bind(word.schema_version as i16)
        .bind(&word.language)
        .bind(kind_string(word.kind))
        .bind(word.revision)
        .bind(headword_mode)
        .bind(source_dialect)
        .bind(word.frequency.as_deref())
        .bind(detection_snapshot)
        .bind(actor_id)
        .bind(word.created_at)
        .execute(&mut **tx)
        .await
        .map_err(map_entry_write_error)?;

        insert_headwords(tx, word).await?;
        insert_forms(tx, word, catalog_parts).await?;
        insert_meanings(tx, word, &HashMap::new()).await?;

        let basics_hash = sha256_json(&word.detection_snapshot)?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_step_progress (
                entry_id, step, completed_revision, content_hash, completed_at
            ) VALUES ($1, 'basics', $2, $3, $4)
            "#,
        )
        .bind(word.id)
        .bind(word.revision)
        .bind(basics_hash)
        .bind(word.created_at)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_editor_projection (
                entry_id, forms, meanings, rebuilt_revision, updated_at
            ) VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(word.id)
        .bind(serde_json::to_value(&word.forms)?)
        .bind(serde_json::to_value(&word.meanings)?)
        .bind(word.revision)
        .bind(word.updated_at)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        let envelope = AdminWordV2Envelope { word: word.clone() };
        sqlx::query(
            r#"
            INSERT INTO platform.idempotency_records (
                scope, idempotency_key, actor_id, request_hash, resource_id,
                response_status, response_body, expires_at
            ) VALUES ('lexicon.entry.create', $1, $2, $3, $4, 201, $5, now() + interval '24 hours')
            "#,
        )
        .bind(idempotency_key)
        .bind(actor_id)
        .bind(request_hash)
        .bind(word.id)
        .bind(serde_json::to_value(envelope)?)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        insert_audit_action(
            tx,
            actor_id,
            "lexicon.entry.create",
            word.id,
            word.revision,
            request_id,
            serde_json::json!({
                "headword_mode": headword_mode,
                "pos_count": word.forms.pos.len()
            }),
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn replace_entry_content(
        tx: &mut Transaction<'_, Postgres>,
        word: &AdminWordV2,
        actor_id: Uuid,
        request_id: Uuid,
        step: &str,
        catalog_parts: &HashMap<String, Uuid>,
        sub_parts: &HashMap<String, Uuid>,
    ) -> Result<(), LexiconRepositoryError> {
        // 当前编辑稿关系表可重建；registry 节点保留并先标记移除，重新出现的稳定 ID 会被激活。
        sqlx::query(
            "UPDATE lexicon.nodes SET removed_from_draft_at = now() WHERE entry_id = $1 AND removed_from_draft_at IS NULL",
        )
        .bind(word.id)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        delete_current_content(tx, word.id).await?;
        insert_forms(tx, word, catalog_parts).await?;
        insert_meanings(tx, word, sub_parts).await?;

        let updated = sqlx::query(
            r#"
            UPDATE lexicon.entries
            SET revision = $2, updated_by_admin_id = $3, updated_at = $4
            WHERE id = $1 AND revision = $5
            "#,
        )
        .bind(word.id)
        .bind(word.revision)
        .bind(actor_id)
        .bind(word.updated_at)
        .bind(word.revision - 1)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        if updated.rows_affected() != 1 {
            return Err(LexiconRepositoryError::Invariant(
                "locked entry revision changed during save",
            ));
        }

        sqlx::query(
            r#"
            UPDATE lexicon.entry_editor_projection
            SET forms = $2, meanings = $3, rebuilt_revision = $4, updated_at = $5
            WHERE entry_id = $1
            "#,
        )
        .bind(word.id)
        .bind(serde_json::to_value(&word.forms)?)
        .bind(serde_json::to_value(&word.meanings)?)
        .bind(word.revision)
        .bind(word.updated_at)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        sqlx::query(
            "DELETE FROM lexicon.entry_step_progress WHERE entry_id = $1 AND step IN ('forms', 'meanings')",
        )
        .bind(word.id)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        for step in &word.completed_steps {
            let (step_name, hash) = match step {
                crate::lexicon::dto::PersistedWordStep::Basics => continue,
                crate::lexicon::dto::PersistedWordStep::Forms => {
                    ("forms", sha256_json(&word.forms)?)
                }
                crate::lexicon::dto::PersistedWordStep::Meanings => {
                    ("meanings", sha256_json(&word.meanings)?)
                }
            };
            sqlx::query(
                r#"
                INSERT INTO lexicon.entry_step_progress (
                    entry_id, step, completed_revision, content_hash, completed_at
                ) VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(word.id)
            .bind(step_name)
            .bind(word.revision)
            .bind(hash)
            .bind(word.updated_at)
            .execute(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)?;
        }

        insert_audit_action(
            tx,
            actor_id,
            "lexicon.entry.save",
            word.id,
            word.revision,
            request_id,
            serde_json::json!({
                "step": step,
                "completed": word.completed_steps.iter().any(|completed| matches!(
                    (step, completed),
                    ("forms", crate::lexicon::dto::PersistedWordStep::Forms)
                        | ("meanings", crate::lexicon::dto::PersistedWordStep::Meanings)
                ))
            }),
        )
        .await
    }
}

// --- entry lookup ---

impl LexiconRepository {
    pub(crate) async fn entry_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<EntryRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, EntryRecord>(
            r#"
            SELECT entry.id,
                   entry.content_schema_version,
                   entry.language,
                   entry.kind,
                   entry.revision,
                   entry.lifecycle_revision,
                   entry.headword_mode,
                   entry.source_dialect,
                   entry.frequency::text AS frequency,
                   entry.detection_snapshot,
                   entry.current_publication_id,
                   publication.source_revision AS current_publication_source_revision,
                   publication.published_at AS current_published_at,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'common') AS common_headword,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'uk') AS uk_headword,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'us') AS us_headword,
                   projection.forms,
                   projection.meanings,
                   COALESCE((
                       SELECT array_agg(progress.step ORDER BY CASE progress.step
                           WHEN 'basics' THEN 1 WHEN 'forms' THEN 2 WHEN 'meanings' THEN 3 END)
                       FROM lexicon.entry_step_progress progress
                       WHERE progress.entry_id = entry.id
                   ), ARRAY[]::text[]) AS completed_steps,
                   entry.created_by_admin_id,
                   entry.created_at,
                   entry.updated_at
                   ,entry.archived_at
                   ,entry.archived_by_admin_id
            FROM lexicon.entries entry
            JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
            LEFT JOIN lexicon.entry_publications publication
              ON publication.id = entry.current_publication_id
             AND publication.entry_id = entry.id
            WHERE entry.id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn entry_by_id_for_update(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<Option<EntryRecord>, LexiconRepositoryError> {
        // 先锁聚合根；后续投影和步骤读取都处于同一事务快照。
        let locked = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM lexicon.entries WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        if locked.is_none() {
            return Ok(None);
        }
        sqlx::query_as::<_, EntryRecord>(
            r#"
            SELECT entry.id,
                   entry.content_schema_version,
                   entry.language,
                   entry.kind,
                   entry.revision,
                   entry.lifecycle_revision,
                   entry.headword_mode,
                   entry.source_dialect,
                   entry.frequency::text AS frequency,
                   entry.detection_snapshot,
                   entry.current_publication_id,
                   publication.source_revision AS current_publication_source_revision,
                   publication.published_at AS current_published_at,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'common') AS common_headword,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'uk') AS uk_headword,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'us') AS us_headword,
                   projection.forms,
                   projection.meanings,
                   COALESCE((
                       SELECT array_agg(progress.step ORDER BY CASE progress.step
                           WHEN 'basics' THEN 1 WHEN 'forms' THEN 2 WHEN 'meanings' THEN 3 END)
                       FROM lexicon.entry_step_progress progress
                       WHERE progress.entry_id = entry.id
                   ), ARRAY[]::text[]) AS completed_steps,
                   entry.created_by_admin_id,
                   entry.created_at,
                   entry.updated_at
                   ,entry.archived_at
                   ,entry.archived_by_admin_id
            FROM lexicon.entries entry
            JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
            LEFT JOIN lexicon.entry_publications publication
              ON publication.id = entry.current_publication_id
             AND publication.entry_id = entry.id
            WHERE entry.id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn delete_never_published_entry(
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        request_id: Uuid,
        id: Uuid,
        revision: i64,
    ) -> Result<bool, LexiconRepositoryError> {
        let has_inbound_draft_references = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM lexicon.relations relation
                JOIN lexicon.nodes target ON target.id = relation.target_sense_id
                WHERE target.entry_id = $1 AND relation.entry_id <> $1
                UNION ALL
                SELECT 1
                FROM lexicon.sentence_links link
                JOIN lexicon.nodes target ON target.id = link.target_sense_id
                WHERE target.entry_id = $1 AND link.entry_id <> $1
            )
            "#,
        )
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        if has_inbound_draft_references {
            return Ok(false);
        }
        delete_current_content(tx, id).await?;
        let deleted = sqlx::query_scalar::<_, Uuid>(
            r#"
            DELETE FROM lexicon.entries entry
            WHERE entry.id = $1
              AND NOT EXISTS (
                  SELECT 1
                  FROM lexicon.entry_publications publication
                  WHERE publication.entry_id = entry.id
              )
            RETURNING entry.id
            "#,
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        if deleted.is_some() {
            insert_audit_action(
                tx,
                actor_id,
                "lexicon.entry.delete_draft",
                id,
                revision,
                request_id,
                serde_json::json!({"never_published": true}),
            )
            .await?;
        }
        Ok(deleted.is_some())
    }
}

// --- persistence ---

pub(super) async fn insert_node(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    entry_id: Uuid,
    node_type: &str,
    parent_node_id: Option<Uuid>,
    node_role: &str,
    stable_slot: bool,
) -> Result<(), LexiconRepositoryError> {
    let result = sqlx::query(
        r#"
        INSERT INTO lexicon.nodes (
            id, entry_id, node_type, parent_node_id, node_role, stable_slot
        )
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (id) DO UPDATE
        SET removed_from_draft_at = NULL
        WHERE lexicon.nodes.entry_id = EXCLUDED.entry_id
          AND lexicon.nodes.node_type = EXCLUDED.node_type
          AND lexicon.nodes.node_role <> 'legacy'
          AND lexicon.nodes.parent_node_id IS NOT DISTINCT FROM EXCLUDED.parent_node_id
          AND lexicon.nodes.node_role = EXCLUDED.node_role
          AND lexicon.nodes.stable_slot = EXCLUDED.stable_slot
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(node_type)
    .bind(parent_node_id)
    .bind(node_role)
    .bind(stable_slot)
    .execute(&mut **tx)
    .await
    .map_err(map_entry_write_error)?;
    if result.rows_affected() != 1 {
        return Err(LexiconRepositoryError::Invariant(
            "node id belongs to another entry, type, parent, or slot",
        ));
    }
    Ok(())
}

pub(super) async fn delete_current_content(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<(), LexiconRepositoryError> {
    for statement in [
        "DELETE FROM lexicon.relations WHERE entry_id = $1",
        "DELETE FROM lexicon.sentence_links WHERE entry_id = $1",
        "DELETE FROM lexicon.text_variants WHERE entry_id = $1",
        "DELETE FROM lexicon.definitions WHERE entry_id = $1",
        "DELETE FROM lexicon.sentences WHERE entry_id = $1",
        "DELETE FROM lexicon.senses WHERE entry_id = $1",
        "DELETE FROM lexicon.grammar_structures WHERE entry_id = $1",
        "DELETE FROM lexicon.sense_groups WHERE entry_id = $1",
        "DELETE FROM lexicon.pronunciations WHERE entry_id = $1",
        "DELETE FROM lexicon.form_variants WHERE entry_id = $1",
        "DELETE FROM lexicon.form_slots WHERE entry_id = $1",
        "DELETE FROM lexicon.form_groups WHERE entry_id = $1",
        "DELETE FROM lexicon.entry_pos WHERE entry_id = $1",
    ] {
        sqlx::query(statement)
            .bind(entry_id)
            .execute(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)?;
    }
    Ok(())
}

pub(super) async fn insert_audit_action(
    tx: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
    action: &str,
    resource_id: Uuid,
    resource_revision: i64,
    request_id: Uuid,
    metadata: serde_json::Value,
) -> Result<(), LexiconRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO audit.admin_actions (
            id, actor_admin_id, action, resource_type, resource_id,
            resource_revision, request_id, metadata
        ) VALUES ($1, $2, $3, 'lexicon.entry', $4, $5, $6, $7)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(actor_id)
    .bind(action)
    .bind(resource_id)
    .bind(resource_revision)
    .bind(request_id)
    .bind(metadata)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(LexiconRepositoryError::Database)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_idempotency_response(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    actor_id: Uuid,
    idempotency_key: Uuid,
    request_hash: &[u8],
    resource_id: Option<Uuid>,
    word: &AdminWordV2,
    response_status: i16,
) -> Result<(), LexiconRepositoryError> {
    insert_idempotency_value(
        tx,
        scope,
        actor_id,
        idempotency_key,
        request_hash,
        resource_id,
        serde_json::to_value(AdminWordV2Envelope { word: word.clone() })?,
        response_status,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_idempotency_value(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    actor_id: Uuid,
    idempotency_key: Uuid,
    request_hash: &[u8],
    resource_id: Option<Uuid>,
    response_body: serde_json::Value,
    response_status: i16,
) -> Result<(), LexiconRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO platform.idempotency_records (
            scope, idempotency_key, actor_id, request_hash, resource_id,
            response_status, response_body, expires_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, now() + interval '24 hours')
        "#,
    )
    .bind(scope)
    .bind(idempotency_key)
    .bind(actor_id)
    .bind(request_hash)
    .bind(resource_id)
    .bind(response_status)
    .bind(response_body)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(LexiconRepositoryError::Database)
}
