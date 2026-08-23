use super::*;

// --- publication ---

impl LexiconRepository {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn insert_publication(
        tx: &mut Transaction<'_, Postgres>,
        word: &AdminWordV2,
        actor_id: Uuid,
        request_id: Uuid,
        idempotency_key: Uuid,
        request_hash: &[u8],
        sense_references: &[NewPublicationSenseReference],
    ) -> Result<Uuid, LexiconRepositoryError> {
        let publication_id = Uuid::now_v7();
        let publication_number = sqlx::query_scalar::<_, i32>(
            "SELECT COALESCE(MAX(publication_number), 0) + 1 FROM lexicon.entry_publications WHERE entry_id = $1",
        )
        .bind(word.id)
        .fetch_one(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        let snapshot = serde_json::to_value(word)?;
        let snapshot_hash = sha256_json(word)?;

        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_publications (
                id, entry_id, publication_number, source_revision,
                content_schema_version, snapshot, snapshot_hash,
                published_by_admin_id, published_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(publication_id)
        .bind(word.id)
        .bind(publication_number)
        .bind(word.revision)
        .bind(word.schema_version as i16)
        .bind(snapshot)
        .bind(snapshot_hash)
        .bind(actor_id)
        .bind(word.published_at.ok_or(LexiconRepositoryError::Invariant(
            "published word has no published_at",
        ))?)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_publication_nodes (
                publication_id, entry_id, node_id, node_type, content_hash
            )
            SELECT $1, node.entry_id, node.id, node.node_type, variant.content_hash
            FROM lexicon.nodes node
            LEFT JOIN lexicon.text_variants variant
              ON variant.id = node.id AND variant.entry_id = node.entry_id
            WHERE node.entry_id = $2 AND node.removed_from_draft_at IS NULL
            "#,
        )
        .bind(publication_id)
        .bind(word.id)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        let mut pronunciation_hashes = Vec::new();
        for pos in &word.forms.pos {
            let mut variants = pos.base_form.variants.iter().collect::<Vec<_>>();
            for group in &pos.form_groups {
                for slot in &group.slots {
                    variants.extend(slot.variants.iter());
                }
            }
            for variant in variants {
                for pronunciation in &variant.pronunciations {
                    pronunciation_hashes.push((
                        pronunciation.id,
                        sha256_json(&serde_json::json!({
                            "spelling": variant.spelling,
                            "dialect": variant.dialect,
                            "dict_phonetic": pronunciation.dict_phonetic,
                            "actual_pron": pronunciation.actual_pron,
                            "style": pronunciation.style,
                        }))?,
                    ));
                }
            }
        }
        for (pronunciation_id, content_hash) in pronunciation_hashes {
            let updated = sqlx::query(
                r#"
                UPDATE lexicon.entry_publication_nodes
                SET content_hash = $3
                WHERE publication_id = $1 AND node_id = $2 AND node_type = 'pronunciation'
                "#,
            )
            .bind(publication_id)
            .bind(pronunciation_id)
            .bind(content_hash)
            .execute(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)?;
            if updated.rows_affected() != 1 {
                return Err(LexiconRepositoryError::Invariant(
                    "publication pronunciation node is missing",
                ));
            }
        }

        for reference in sense_references {
            if reference.target_entry_id == word.id {
                return Err(LexiconRepositoryError::Invariant(
                    "publication sense reference must target another entry",
                ));
            }
            sqlx::query(
                r#"
                INSERT INTO lexicon.entry_publication_sense_refs (
                    publication_id, entry_id, source_node_id, reference_kind,
                    target_entry_id, target_sense_id, target_publication_id,
                    target_content_scope, target_revision
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                "#,
            )
            .bind(publication_id)
            .bind(word.id)
            .bind(reference.source_node_id)
            .bind(reference.reference_kind.as_str())
            .bind(reference.target_entry_id)
            .bind(reference.target_sense_id)
            .bind(reference.target_publication_id)
            .bind(reference.target_content_scope.as_str())
            .bind(reference.target_revision)
            .execute(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)?;
        }

        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_publication_part_of_speech_refs (
                publication_id, entry_id, source_node_id, part_of_speech_id
            )
            SELECT $1, entry_id, id, part_of_speech_id
            FROM lexicon.entry_pos WHERE entry_id = $2
            "#,
        )
        .bind(publication_id)
        .bind(word.id)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_publication_sub_part_of_speech_refs (
                publication_id, entry_id, source_node_id, sub_part_of_speech_id
            )
            SELECT $1, entry_id, id, sub_part_of_speech_id
            FROM lexicon.senses
            WHERE entry_id = $2 AND sub_part_of_speech_id IS NOT NULL
            "#,
        )
        .bind(publication_id)
        .bind(word.id)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        sqlx::query(
            "UPDATE lexicon.entries SET current_publication_id = $2, draft_based_on_publication_id = $2 WHERE id = $1",
        )
        .bind(word.id)
        .bind(publication_id)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        sqlx::query(
            "UPDATE lexicon.nodes SET first_published_at = COALESCE(first_published_at, $2) WHERE entry_id = $1 AND removed_from_draft_at IS NULL",
        )
        .bind(word.id)
        .bind(word.published_at)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        sqlx::query(
            r#"
            INSERT INTO platform.outbox_events (
                id, aggregate_type, aggregate_id, aggregate_revision,
                event_type, payload, occurred_at, available_at
            ) VALUES ($1, 'lexicon.entry', $2, $3, 'lexicon.entry_published', $4, $5, $5)
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(word.id)
        .bind(word.revision)
        .bind(serde_json::json!({
            "entry_id": word.id,
            "publication_id": publication_id,
            "publication_number": publication_number,
        }))
        .bind(word.published_at)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        insert_audit_action(
            tx,
            actor_id,
            "lexicon.entry.publish",
            word.id,
            word.revision,
            request_id,
            serde_json::json!({
                "publication_id": publication_id,
                "publication_number": publication_number,
            }),
        )
        .await?;
        insert_idempotency_response(
            tx,
            "lexicon.entry.publish",
            actor_id,
            idempotency_key,
            request_hash,
            Some(publication_id),
            word,
            201,
        )
        .await?;
        Ok(publication_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn insert_idempotent_word_response(
        tx: &mut Transaction<'_, Postgres>,
        scope: &str,
        actor_id: Uuid,
        idempotency_key: Uuid,
        request_hash: &[u8],
        resource_id: Option<Uuid>,
        word: &AdminWordV2,
        response_status: i16,
    ) -> Result<(), LexiconRepositoryError> {
        insert_idempotency_response(
            tx,
            scope,
            actor_id,
            idempotency_key,
            request_hash,
            resource_id,
            word,
            response_status,
        )
        .await
    }

    pub(crate) async fn historical_publication_for_update(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        publication_id: Uuid,
    ) -> Result<Option<HistoricalPublicationRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, HistoricalPublicationRecord>(
            r#"
            SELECT id, entry_id, publication_number, source_revision, snapshot, published_at
            FROM lexicon.entry_publications
            WHERE id = $1 AND entry_id = $2
            FOR UPDATE
            "#,
        )
        .bind(publication_id)
        .bind(entry_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// `(entry_id, source_revision)` 上有唯一约束，同一 revision 至多一条 publication。
    pub(crate) async fn publication_by_source_revision_for_update(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        source_revision: i64,
    ) -> Result<Option<HistoricalPublicationRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, HistoricalPublicationRecord>(
            r#"
            SELECT id, entry_id, publication_number, source_revision, snapshot, published_at
            FROM lexicon.entry_publications
            WHERE entry_id = $1 AND source_revision = $2
            FOR UPDATE
            "#,
        )
        .bind(entry_id)
        .bind(source_revision)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// 把一条已存在的 publication 设为 current publication。
    ///
    /// `scope` 同时用作幂等作用域与审计动作：显式激活走
    /// `lexicon.publication.activate`，回滚后的「再发布一次」走
    /// `lexicon.entry.publish`，两者写的是各自入口的幂等记录。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn activate_historical_publication(
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        request_id: Uuid,
        scope: &str,
        response_status: i16,
        idempotency_key: Uuid,
        request_hash: &[u8],
        publication: &HistoricalPublicationRecord,
        previous_publication_id: Option<Uuid>,
        word: &AdminWordV2,
    ) -> Result<(), LexiconRepositoryError> {
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.entries
            SET current_publication_id = $2,
                updated_by_admin_id = $3,
                updated_at = $4,
                lifecycle_revision = $6
            WHERE id = $1 AND revision = $5 AND lifecycle_revision = $7
            "#,
        )
        .bind(word.id)
        .bind(publication.id)
        .bind(actor_id)
        .bind(word.updated_at)
        .bind(word.revision)
        .bind(word.lifecycle_revision)
        .bind(word.lifecycle_revision - 1)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        if updated.rows_affected() != 1 {
            return Err(LexiconRepositoryError::Invariant(
                "locked entry revision changed during publication activation",
            ));
        }

        sqlx::query(
            r#"
            INSERT INTO platform.outbox_events (
                id, aggregate_type, aggregate_id, aggregate_revision,
                event_type, payload, occurred_at, available_at
            ) VALUES (
                $1, 'lexicon.entry', $2, $3,
                'lexicon.publication_activated', $4, $5, $5
            )
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(word.id)
        .bind(word.lifecycle_revision)
        .bind(serde_json::json!({
            "entry_id": word.id,
            "publication_id": publication.id,
            "publication_number": publication.publication_number,
            "previous_publication_id": previous_publication_id,
            "lifecycle_revision": word.lifecycle_revision,
        }))
        .bind(word.updated_at)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        insert_audit_action(
            tx,
            actor_id,
            scope,
            word.id,
            word.revision,
            request_id,
            serde_json::json!({
                "publication_id": publication.id,
                "publication_number": publication.publication_number,
                "previous_publication_id": previous_publication_id,
            }),
        )
        .await?;
        insert_idempotency_response(
            tx,
            scope,
            actor_id,
            idempotency_key,
            request_hash,
            Some(publication.id),
            word,
            response_status,
        )
        .await
    }
}

// --- references ---

impl LexiconRepository {
    pub(crate) async fn node_identities(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<NodeIdentityRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, NodeIdentityRecord>(
            r#"
            SELECT id, entry_id, node_type, parent_node_id, node_role, stable_slot
            FROM lexicon.nodes
            WHERE entry_id = $1 OR id = ANY($2)
            "#,
        )
        .bind(entry_id)
        .bind(ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// Serialize all client-provided node IDs before checking their ownership.
    ///
    /// The ownership query alone cannot see an uncommitted insert from another
    /// entry. Taking deterministic transaction-scoped advisory locks closes
    /// that race without retaining tombstone rows for IDs that never save.
    pub(crate) async fn lock_node_ids(
        tx: &mut Transaction<'_, Postgres>,
        ids: &[Uuid],
    ) -> Result<(), LexiconRepositoryError> {
        if ids.is_empty() {
            return Ok(());
        }
        sqlx::query(
            r#"
            SELECT pg_advisory_xact_lock(
                hashtextextended('lexicon.node:' || requested.node_id::text, 0)
            )
            FROM (
                SELECT DISTINCT node_id
                FROM unnest($1::uuid[]) AS value(node_id)
                ORDER BY node_id
            ) requested
            "#,
        )
        .bind(ids)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        Ok(())
    }

    pub(crate) async fn resolve_current_published_senses(
        tx: &mut Transaction<'_, Postgres>,
        targets: &[SenseTargetKey],
    ) -> Result<Vec<ResolvedSenseTargetRecord>, LexiconRepositoryError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let target_entry_ids = targets
            .iter()
            .map(|target| target.target_entry_id)
            .collect::<Vec<_>>();
        let target_sense_ids = targets
            .iter()
            .map(|target| target.target_sense_id)
            .collect::<Vec<_>>();
        sqlx::query_as::<_, ResolvedSenseTargetRecord>(
            r#"
            WITH requested AS (
                SELECT DISTINCT target_entry_id, target_sense_id
                FROM unnest($1::uuid[], $2::uuid[])
                    AS target(target_entry_id, target_sense_id)
            )
            SELECT requested.target_entry_id,
                   requested.target_sense_id,
                   publication.id AS target_publication_id,
                   publication.source_revision AS target_revision,
                   publication.snapshot
            FROM requested
            JOIN lexicon.entries entry
              ON entry.id = requested.target_entry_id
             AND entry.archived_at IS NULL
            JOIN lexicon.entry_publications publication
              ON publication.id = entry.current_publication_id
             AND publication.entry_id = entry.id
            JOIN lexicon.entry_publication_nodes publication_node
              ON publication_node.publication_id = publication.id
             AND publication_node.entry_id = entry.id
             AND publication_node.node_id = requested.target_sense_id
             AND publication_node.node_type = 'sense'
            ORDER BY requested.target_entry_id, requested.target_sense_id
            "#,
        )
        .bind(target_entry_ids)
        .bind(target_sense_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn resolve_relation_targets(
        tx: &mut Transaction<'_, Postgres>,
        targets: &[SenseTargetKey],
    ) -> Result<Vec<ResolvedRelationTargetRecord>, LexiconRepositoryError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let target_entry_ids = targets
            .iter()
            .map(|target| target.target_entry_id)
            .collect::<Vec<_>>();
        let target_sense_ids = targets
            .iter()
            .map(|target| target.target_sense_id)
            .collect::<Vec<_>>();
        sqlx::query_as::<_, ResolvedRelationTargetRecord>(
            r#"
            WITH requested AS (
                SELECT DISTINCT target_entry_id, target_sense_id
                FROM unnest($1::uuid[], $2::uuid[])
                    AS target(target_entry_id, target_sense_id)
            )
            SELECT requested.target_entry_id,
                   requested.target_sense_id,
                   entry.revision AS target_revision,
                   entry.archived_at IS NOT NULL AS target_archived,
                   node.removed_from_draft_at IS NOT NULL AS target_removed,
                   entry.headword_mode,
                   entry.source_dialect,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'common') AS common_headword,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'uk') AS uk_headword,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'us') AS us_headword,
                   projection.meanings AS draft_meanings,
                   CASE WHEN publication_node.node_id IS NOT NULL
                        THEN publication.id END AS target_publication_id,
                   CASE WHEN publication_node.node_id IS NOT NULL
                        THEN publication.snapshot END AS published_snapshot,
                   CASE WHEN publication_node.node_id IS NOT NULL
                        THEN publication.source_revision END AS published_revision
            FROM requested
            JOIN lexicon.nodes node
              ON node.id = requested.target_sense_id
             AND node.entry_id = requested.target_entry_id
             AND node.node_type = 'sense'
            JOIN lexicon.entries entry ON entry.id = requested.target_entry_id
            JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
            LEFT JOIN lexicon.entry_publications publication
              ON publication.id = entry.current_publication_id
             AND publication.entry_id = entry.id
            LEFT JOIN lexicon.entry_publication_nodes publication_node
              ON publication_node.publication_id = publication.id
             AND publication_node.entry_id = entry.id
             AND publication_node.node_id = requested.target_sense_id
             AND publication_node.node_type = 'sense'
            ORDER BY requested.target_entry_id, requested.target_sense_id
            "#,
        )
        .bind(target_entry_ids)
        .bind(target_sense_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn resolve_relation_targets_for_publish(
        tx: &mut Transaction<'_, Postgres>,
        targets: &[SenseTargetKey],
    ) -> Result<Vec<ResolvedRelationTargetRecord>, LexiconRepositoryError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let target_entry_ids = targets
            .iter()
            .map(|target| target.target_entry_id)
            .collect::<Vec<_>>();
        let target_sense_ids = targets
            .iter()
            .map(|target| target.target_sense_id)
            .collect::<Vec<_>>();
        sqlx::query_as::<_, ResolvedRelationTargetRecord>(
            r#"
            WITH requested AS (
                SELECT DISTINCT target_entry_id, target_sense_id
                FROM unnest($1::uuid[], $2::uuid[])
                    AS target(target_entry_id, target_sense_id)
            )
            SELECT requested.target_entry_id,
                   requested.target_sense_id,
                   entry.revision AS target_revision,
                   entry.archived_at IS NOT NULL AS target_archived,
                   node.removed_from_draft_at IS NOT NULL AS target_removed,
                   entry.headword_mode,
                   entry.source_dialect,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'common') AS common_headword,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'uk') AS uk_headword,
                   (SELECT headword FROM lexicon.entry_headwords
                    WHERE entry_id = entry.id AND dialect = 'us') AS us_headword,
                   projection.meanings AS draft_meanings,
                   CASE WHEN publication_node.node_id IS NOT NULL
                        THEN publication.id END AS target_publication_id,
                   CASE WHEN publication_node.node_id IS NOT NULL
                        THEN publication.snapshot END AS published_snapshot,
                   CASE WHEN publication_node.node_id IS NOT NULL
                        THEN publication.source_revision END AS published_revision
            FROM requested
            JOIN lexicon.nodes node
              ON node.id = requested.target_sense_id
             AND node.entry_id = requested.target_entry_id
             AND node.node_type = 'sense'
            JOIN lexicon.entries entry ON entry.id = requested.target_entry_id
            JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
            LEFT JOIN lexicon.entry_publications publication
              ON publication.id = entry.current_publication_id
             AND publication.entry_id = entry.id
            LEFT JOIN lexicon.entry_publication_nodes publication_node
              ON publication_node.publication_id = publication.id
             AND publication_node.entry_id = entry.id
             AND publication_node.node_id = requested.target_sense_id
             AND publication_node.node_type = 'sense'
            ORDER BY requested.target_entry_id, requested.target_sense_id
            FOR SHARE OF entry NOWAIT
            "#,
        )
        .bind(target_entry_ids)
        .bind(target_sense_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_target_publication_lock_error)
    }

    pub(crate) async fn resolve_current_published_senses_for_publish(
        tx: &mut Transaction<'_, Postgres>,
        targets: &[SenseTargetKey],
    ) -> Result<Vec<ResolvedSenseTargetRecord>, LexiconRepositoryError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let target_entry_ids = targets
            .iter()
            .map(|target| target.target_entry_id)
            .collect::<Vec<_>>();
        let target_sense_ids = targets
            .iter()
            .map(|target| target.target_sense_id)
            .collect::<Vec<_>>();
        sqlx::query_as::<_, ResolvedSenseTargetRecord>(
            r#"
            WITH requested AS (
                SELECT DISTINCT target_entry_id, target_sense_id
                FROM unnest($1::uuid[], $2::uuid[])
                    AS target(target_entry_id, target_sense_id)
            )
            SELECT requested.target_entry_id,
                   requested.target_sense_id,
                   publication.id AS target_publication_id,
                   publication.source_revision AS target_revision,
                   publication.snapshot
            FROM requested
            JOIN lexicon.entries entry
              ON entry.id = requested.target_entry_id
             AND entry.archived_at IS NULL
            JOIN lexicon.entry_publications publication
              ON publication.id = entry.current_publication_id
             AND publication.entry_id = entry.id
            JOIN lexicon.entry_publication_nodes publication_node
              ON publication_node.publication_id = publication.id
             AND publication_node.entry_id = entry.id
             AND publication_node.node_id = requested.target_sense_id
             AND publication_node.node_type = 'sense'
            ORDER BY requested.target_entry_id, requested.target_sense_id
            FOR SHARE OF entry NOWAIT
            "#,
        )
        .bind(target_entry_ids)
        .bind(target_sense_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_target_publication_lock_error)
    }

    pub(crate) async fn current_inbound_sense_refs(
        tx: &mut Transaction<'_, Postgres>,
        target_entry_id: Uuid,
        retained_sense_ids: &[Uuid],
    ) -> Result<Vec<InboundSenseReferenceRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, InboundSenseReferenceRecord>(
            r#"
            SELECT sense_ref.target_sense_id,
                   sense_ref.entry_id AS source_entry_id,
                   sense_ref.publication_id AS source_publication_id,
                   sense_ref.source_node_id,
                   sense_ref.reference_kind
            FROM lexicon.entry_publication_sense_refs sense_ref
            JOIN lexicon.entries source_entry
              ON source_entry.id = sense_ref.entry_id
             AND source_entry.current_publication_id = sense_ref.publication_id
             AND source_entry.archived_at IS NULL
            WHERE sense_ref.target_entry_id = $1
              AND NOT (sense_ref.target_sense_id = ANY($2::uuid[]))
            ORDER BY sense_ref.target_sense_id,
                     sense_ref.entry_id,
                     sense_ref.source_node_id
            FOR SHARE OF source_entry NOWAIT
            "#,
        )
        .bind(target_entry_id)
        .bind(retained_sense_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_target_publication_lock_error)
    }

    pub(crate) async fn active_inbound_sense_refs(
        tx: &mut Transaction<'_, Postgres>,
        target_entry_id: Uuid,
        excluding_source_entry_ids: &[Uuid],
    ) -> Result<Vec<InboundSenseReferenceRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, InboundSenseReferenceRecord>(
            r#"
            SELECT sense_ref.target_sense_id,
                   sense_ref.entry_id AS source_entry_id,
                   sense_ref.publication_id AS source_publication_id,
                   sense_ref.source_node_id,
                   sense_ref.reference_kind
            FROM lexicon.entry_publication_sense_refs sense_ref
            JOIN lexicon.entries source_entry
              ON source_entry.id = sense_ref.entry_id
             AND source_entry.current_publication_id = sense_ref.publication_id
             AND source_entry.archived_at IS NULL
            WHERE sense_ref.target_entry_id = $1
              AND NOT (sense_ref.entry_id = ANY($2::uuid[]))
            ORDER BY sense_ref.target_sense_id,
                     sense_ref.entry_id,
                     sense_ref.source_node_id
            FOR SHARE OF source_entry NOWAIT
            "#,
        )
        .bind(target_entry_id)
        .bind(excluding_source_entry_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_target_publication_lock_error)
    }

    pub(crate) async fn unavailable_outbound_sense_refs_for_restore(
        tx: &mut Transaction<'_, Postgres>,
        source_entry_id: Uuid,
        restoring_entry_ids: &[Uuid],
    ) -> Result<Vec<InboundSenseReferenceRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, InboundSenseReferenceRecord>(
            r#"
            SELECT sense_ref.target_sense_id,
                   sense_ref.entry_id AS source_entry_id,
                   sense_ref.publication_id AS source_publication_id,
                   sense_ref.source_node_id,
                   sense_ref.reference_kind
            FROM lexicon.entry_publication_sense_refs sense_ref
            JOIN lexicon.entries source_entry
              ON source_entry.id = sense_ref.entry_id
             AND source_entry.current_publication_id = sense_ref.publication_id
            JOIN lexicon.entries target_entry
              ON target_entry.id = sense_ref.target_entry_id
            LEFT JOIN lexicon.entry_publication_nodes target_node
              ON target_node.publication_id = target_entry.current_publication_id
             AND target_node.entry_id = target_entry.id
             AND target_node.node_id = sense_ref.target_sense_id
             AND target_node.node_type = 'sense'
            LEFT JOIN lexicon.nodes target_draft_node
              ON target_draft_node.id = sense_ref.target_sense_id
             AND target_draft_node.entry_id = target_entry.id
             AND target_draft_node.node_type = 'sense'
            WHERE sense_ref.entry_id = $1
              AND (
                   (
                       target_entry.archived_at IS NOT NULL
                       AND NOT (target_entry.id = ANY($2::uuid[]))
                   )
                   OR (
                       sense_ref.reference_kind = 'sentence_context'
                       AND target_node.node_id IS NULL
                   )
                   OR (
                       sense_ref.reference_kind = 'relation'
                       AND target_node.node_id IS NULL
                       AND (
                           target_draft_node.id IS NULL
                           OR target_draft_node.removed_from_draft_at IS NOT NULL
                       )
                   )
              )
            ORDER BY sense_ref.target_sense_id,
                     sense_ref.entry_id,
                     sense_ref.source_node_id
            "#,
        )
        .bind(source_entry_id)
        .bind(restoring_entry_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn unavailable_outbound_sense_refs_for_publication(
        tx: &mut Transaction<'_, Postgres>,
        publication_id: Uuid,
    ) -> Result<Vec<InboundSenseReferenceRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, InboundSenseReferenceRecord>(
            r#"
            SELECT sense_ref.target_sense_id,
                   sense_ref.entry_id AS source_entry_id,
                   sense_ref.publication_id AS source_publication_id,
                   sense_ref.source_node_id,
                   sense_ref.reference_kind
            FROM lexicon.entry_publication_sense_refs sense_ref
            JOIN lexicon.entries target_entry
              ON target_entry.id = sense_ref.target_entry_id
            LEFT JOIN lexicon.entry_publication_nodes target_node
              ON target_node.publication_id = target_entry.current_publication_id
             AND target_node.entry_id = target_entry.id
             AND target_node.node_id = sense_ref.target_sense_id
             AND target_node.node_type = 'sense'
            LEFT JOIN lexicon.nodes target_draft_node
              ON target_draft_node.id = sense_ref.target_sense_id
             AND target_draft_node.entry_id = target_entry.id
             AND target_draft_node.node_type = 'sense'
            WHERE sense_ref.publication_id = $1
              AND (
                   target_entry.archived_at IS NOT NULL
                   OR (
                       sense_ref.reference_kind = 'sentence_context'
                       AND target_node.node_id IS NULL
                   )
                   OR (
                       sense_ref.reference_kind = 'relation'
                       AND target_node.node_id IS NULL
                       AND (
                           target_draft_node.id IS NULL
                           OR target_draft_node.removed_from_draft_at IS NOT NULL
                       )
                   )
              )
            ORDER BY sense_ref.target_sense_id,
                     sense_ref.entry_id,
                     sense_ref.source_node_id
            "#,
        )
        .bind(publication_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn lock_outbound_sense_ref_targets_for_publication(
        tx: &mut Transaction<'_, Postgres>,
        publication_id: Uuid,
    ) -> Result<(), LexiconRepositoryError> {
        sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT target_entry.id
            FROM lexicon.entry_publication_sense_refs sense_ref
            JOIN lexicon.entries target_entry
              ON target_entry.id = sense_ref.target_entry_id
            WHERE sense_ref.publication_id = $1
            ORDER BY target_entry.id
            FOR SHARE OF target_entry NOWAIT
            "#,
        )
        .bind(publication_id)
        .fetch_all(&mut **tx)
        .await
        .map(|_| ())
        .map_err(map_target_publication_lock_error)
    }

    pub(crate) async fn publication_relation_target_entry_ids(
        tx: &mut Transaction<'_, Postgres>,
        publication_ids: &[Uuid],
    ) -> Result<Vec<Uuid>, LexiconRepositoryError> {
        if publication_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT DISTINCT sense_ref.target_entry_id
            FROM lexicon.entry_publication_sense_refs sense_ref
            WHERE sense_ref.publication_id = ANY($1::uuid[])
              AND sense_ref.reference_kind = 'relation'
            ORDER BY sense_ref.target_entry_id
            "#,
        )
        .bind(publication_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn current_publication_relation_target_entry_ids(
        tx: &mut Transaction<'_, Postgres>,
        entry_ids: &[Uuid],
    ) -> Result<Vec<Uuid>, LexiconRepositoryError> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT DISTINCT sense_ref.target_entry_id
            FROM lexicon.entries source_entry
            JOIN lexicon.entry_publication_sense_refs sense_ref
              ON sense_ref.publication_id = source_entry.current_publication_id
             AND sense_ref.entry_id = source_entry.id
            WHERE source_entry.id = ANY($1::uuid[])
              AND sense_ref.reference_kind = 'relation'
            ORDER BY sense_ref.target_entry_id
            "#,
        )
        .bind(entry_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn lock_current_outbound_sense_ref_targets_for_entry(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
    ) -> Result<(), LexiconRepositoryError> {
        sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT target_entry.id
            FROM lexicon.entries source_entry
            JOIN lexicon.entry_publication_sense_refs sense_ref
              ON sense_ref.publication_id = source_entry.current_publication_id
             AND sense_ref.entry_id = source_entry.id
            JOIN lexicon.entries target_entry
              ON target_entry.id = sense_ref.target_entry_id
            WHERE source_entry.id = $1
            ORDER BY target_entry.id
            FOR SHARE OF target_entry NOWAIT
            "#,
        )
        .bind(entry_id)
        .fetch_all(&mut **tx)
        .await
        .map(|_| ())
        .map_err(map_target_publication_lock_error)
    }
}
