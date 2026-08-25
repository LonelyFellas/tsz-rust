use super::*;

impl LexiconRepository {
    pub(crate) async fn lifecycle_schema_versions(
        &self,
        entry_ids: &[Uuid],
    ) -> Result<Vec<i16>, LexiconRepositoryError> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_scalar(
            r#"
            SELECT content_schema_version
            FROM lexicon.entries
            WHERE id = ANY($1)
            ORDER BY id
            "#,
        )
        .bind(entry_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn lifecycle_surface_lock_keys(
        tx: &mut Transaction<'_, Postgres>,
        entry_ids: &[Uuid],
    ) -> Result<Vec<SurfaceLockKey>, LexiconRepositoryError> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, (String, String, String)>(
            r#"
            SELECT DISTINCT source.language, source.dialect_scope,
                            source.normalized_surface
            FROM lexicon.surface_sources source
            JOIN lexicon.entries entry ON entry.id = source.entry_id
            WHERE source.entry_id = ANY($1)
              AND source.content_schema_version = 3
              AND source.is_deleted = FALSE
              AND (
                  source.content_scope = 'draft'
                  OR (
                      source.content_scope = 'current_publication'
                      AND source.publication_id = entry.current_publication_id
                  )
              )
            ORDER BY source.language, source.dialect_scope, source.normalized_surface
            "#,
        )
        .bind(entry_ids)
        .fetch_all(&mut **tx)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(
                    |(language, dialect_scope, normalized_surface)| SurfaceLockKey {
                        language,
                        dialect_scope,
                        normalized_surface,
                    },
                )
                .collect()
        })
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn lifecycle_v2_publication_surface_evidence(
        tx: &mut Transaction<'_, Postgres>,
        entry_ids: &[Uuid],
    ) -> Result<Vec<serde_json::Value>, LexiconRepositoryError> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_scalar(
            r#"
            SELECT jsonb_build_object(
                'entry_id', source.entry_id,
                'publication_id', source.publication_id,
                'source_id', source.source_id,
                'source_kind', source.source_kind,
                'source_node_id', source.source_node_id,
                'language', source.language,
                'entry_kind', source.entry_kind,
                'dialect', source.dialect,
                'dialect_scope', source.dialect_scope,
                'surface', source.surface,
                'normalized_surface', source.normalized_surface,
                'normalization_version', source.normalization_version,
                'source_revision', source.source_revision,
                'event_offset', source.event_offset,
                'content_scope', source.content_scope,
                'pos_id', source.pos_id,
                'pos', source.pos,
                'form_type', source.form_type,
                'content_schema_version', source.content_schema_version
            )
            FROM lexicon.surface_sources source
            JOIN lexicon.entries entry ON entry.id = source.entry_id
            WHERE source.entry_id = ANY($1)
              AND source.content_schema_version = 2
              AND source.content_scope = 'current_publication'
              AND source.publication_id = entry.current_publication_id
              AND source.is_deleted = FALSE
            ORDER BY source.entry_id, source.source_id, source.dialect_scope,
                     source.normalization_version, source.event_offset
            "#,
        )
        .bind(entry_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn retire_v3_draft_surface_projection(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        source_revision: i64,
    ) -> Result<(), LexiconRepositoryError> {
        let event_offset = sqlx::query_scalar::<_, i64>(
            "SELECT nextval('lexicon.surface_projection_event_offset_seq')",
        )
        .fetch_one(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        sqlx::query(
            r#"
            UPDATE lexicon.surface_sources
            SET source_revision = $2,
                event_offset = $3,
                is_deleted = TRUE,
                updated_at = now()
            WHERE entry_id = $1
              AND content_schema_version = 3
              AND content_scope = 'draft'
              AND (source_revision, event_offset) <= ($2, $3)
            "#,
        )
        .bind(entry_id)
        .bind(source_revision)
        .bind(event_offset)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        sqlx::query(
            r#"
            INSERT INTO platform.outbox_events (
                id, aggregate_type, aggregate_id, aggregate_revision,
                event_type, payload, occurred_at, available_at
            ) VALUES (
                $1, 'lexicon.surface_projection', $2, $3,
                'lexicon.surface_projection_replaced', $4, now(), now()
            )
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(entry_id)
        .bind(event_offset)
        .bind(serde_json::json!({
            "entry_id": entry_id,
            "content_schema_version": 3,
            "content_scope": "draft",
            "publication_id": Option::<Uuid>::None,
            "source_revision": source_revision,
            "event_offset": event_offset,
            "source_count": 0,
        }))
        .execute(&mut **tx)
        .await
        .map(|_| ())
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn transition_lifecycle(
        tx: &mut Transaction<'_, Postgres>,
        word: &AdminWordAny,
        actor_id: Uuid,
        request_id: Uuid,
    ) -> Result<(), LexiconRepositoryError> {
        let (id, revision, lifecycle_revision, status, updated_at, published_revision) = match word
        {
            AdminWordAny::V2(word) => (
                word.id,
                word.revision,
                word.lifecycle_revision,
                word.status,
                word.updated_at,
                word.published_revision,
            ),
            AdminWordAny::V3(word) => (
                word.id,
                word.revision,
                word.lifecycle_revision,
                word.status,
                word.updated_at,
                word.published_revision,
            ),
        };
        let archived = matches!(status, crate::lexicon::dto::AdminWordStatus::Archived);
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.entries
            SET lifecycle_revision = $2,
                archived_at = CASE WHEN $3 THEN $4 ELSE NULL END,
                archived_by_admin_id = CASE WHEN $3 THEN $5 ELSE NULL END,
                updated_by_admin_id = $5,
                updated_at = $4
            WHERE id = $1 AND lifecycle_revision = $6 AND revision = $7
            "#,
        )
        .bind(id)
        .bind(lifecycle_revision)
        .bind(archived)
        .bind(updated_at)
        .bind(actor_id)
        .bind(lifecycle_revision - 1)
        .bind(revision)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        if updated.rows_affected() != 1 {
            return Err(LexiconRepositoryError::Invariant(
                "locked entry lifecycle revision changed during transition",
            ));
        }

        let event_type = if archived {
            "lexicon.entry_archived"
        } else {
            "lexicon.entry_restored"
        };
        sqlx::query(
            r#"
            INSERT INTO platform.outbox_events (
                id, aggregate_type, aggregate_id, aggregate_revision,
                event_type, payload, occurred_at, available_at
            ) VALUES ($1, 'lexicon.entry.lifecycle', $2, $3, $4, $5, $6, $6)
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(id)
        .bind(lifecycle_revision)
        .bind(event_type)
        .bind(serde_json::json!({
            "entry_id": id,
            "lifecycle_revision": lifecycle_revision,
            "archived": archived,
            "has_current_publication": published_revision.is_some(),
        }))
        .bind(updated_at)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        insert_audit_action(
            tx,
            actor_id,
            if archived {
                "lexicon.entry.archive"
            } else {
                "lexicon.entry.restore"
            },
            id,
            revision,
            request_id,
            serde_json::json!({
                "lifecycle_revision": lifecycle_revision,
                "current_publication_preserved": published_revision.is_some(),
            }),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn insert_idempotent_response<T: serde::Serialize>(
        tx: &mut Transaction<'_, Postgres>,
        scope: &str,
        actor_id: Uuid,
        idempotency_key: Uuid,
        request_hash: &[u8],
        resource_id: Option<Uuid>,
        response: &T,
        response_status: i16,
    ) -> Result<(), LexiconRepositoryError> {
        insert_idempotency_value(
            tx,
            scope,
            actor_id,
            idempotency_key,
            request_hash,
            resource_id,
            serde_json::to_value(response)?,
            response_status,
        )
        .await
    }
}
