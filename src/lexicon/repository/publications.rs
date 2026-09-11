use super::*;

// --- publication ---

impl LexiconRepository {
    pub(crate) async fn publication_history(
        &self,
        entry_id: Uuid,
    ) -> Result<Option<Vec<PublicationReadRecord>>, LexiconRepositoryError> {
        let entry_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM lexicon.entries WHERE id = $1)",
        )
        .bind(entry_id)
        .fetch_one(self.pool())
        .await
        .map_err(LexiconRepositoryError::Database)?;
        if !entry_exists {
            return Ok(None);
        }
        sqlx::query_as::<_, PublicationReadRecord>(
            r#"
            SELECT publication.id,
                   publication.entry_id,
                   publication.publication_number,
                   publication.source_revision,
                   publication.content_schema_version,
                   publication.snapshot,
                   publication.published_by_admin_id,
                   publication.published_at,
                   entry.current_publication_id = publication.id AS is_current
            FROM lexicon.entry_publications publication
            JOIN lexicon.entries entry ON entry.id = publication.entry_id
            WHERE publication.entry_id = $1
            ORDER BY publication.publication_number DESC
            "#,
        )
        .bind(entry_id)
        .fetch_all(self.pool())
        .await
        .map(Some)
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn publication(
        &self,
        entry_id: Uuid,
        publication_id: Uuid,
    ) -> Result<Option<PublicationReadRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, PublicationReadRecord>(
            r#"
            SELECT publication.id,
                   publication.entry_id,
                   publication.publication_number,
                   publication.source_revision,
                   publication.content_schema_version,
                   publication.snapshot,
                   publication.published_by_admin_id,
                   publication.published_at,
                   entry.current_publication_id = publication.id AS is_current
            FROM lexicon.entry_publications publication
            JOIN lexicon.entries entry ON entry.id = publication.entry_id
            WHERE publication.entry_id = $1 AND publication.id = $2
            "#,
        )
        .bind(entry_id)
        .bind(publication_id)
        .fetch_optional(self.pool())
        .await
        .map_err(LexiconRepositoryError::Database)
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
                   entry.content_schema_version,
                   presentation.label AS presentation_label,
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
            LEFT JOIN lexicon.entry_presentation_projection presentation
              ON presentation.entry_id = entry.id
             AND presentation.content_schema_version = 3
             AND presentation.source_revision = entry.revision
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
                   entry.content_schema_version,
                   presentation.label AS presentation_label,
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
            LEFT JOIN lexicon.entry_presentation_projection presentation
              ON presentation.entry_id = entry.id
             AND presentation.content_schema_version = 3
             AND presentation.source_revision = entry.revision
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
                       -- 短语成分与关联词同款：目标词义既不在目标当前发布里、
                       -- 草稿侧也已消失，这条出引用就悬空了。
                       sense_ref.reference_kind IN ('relation', 'phrase_component', 'text_link')
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
                       -- 短语成分与关联词同款：目标词义既不在目标当前发布里、
                       -- 草稿侧也已消失，这条出引用就悬空了。
                       sense_ref.reference_kind IN ('relation', 'phrase_component', 'text_link')
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

    /// 发布时核验草稿范围的目标词义（正文关联与短语成分共用）：目标词条行 `FOR SHARE NOWAIT` 锁住，
    /// 带回当时的 entry revision；归档 / 词义已从草稿移除由调用方按可用性处理。
    pub(crate) async fn draft_sense_targets_for_publish(
        tx: &mut Transaction<'_, Postgres>,
        targets: &[SenseTargetKey],
    ) -> Result<Vec<DraftSenseTargetRecord>, LexiconRepositoryError> {
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
        sqlx::query_as::<_, DraftSenseTargetRecord>(
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
                   node.removed_from_draft_at IS NOT NULL AS target_removed
            FROM requested
            JOIN lexicon.nodes node
              ON node.id = requested.target_sense_id
             AND node.entry_id = requested.target_entry_id
             AND node.node_type = 'sense'
            JOIN lexicon.entries entry
              ON entry.id = requested.target_entry_id
             AND entry.content_schema_version = 3
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

    pub(crate) async fn phrase_component_publication_targets_for_publish(
        tx: &mut Transaction<'_, Postgres>,
        target_entry_ids: &[Uuid],
        target_publication_ids: &[Uuid],
        target_sense_ids: &[Uuid],
    ) -> Result<Vec<(Uuid, Uuid, Uuid, i64)>, LexiconRepositoryError> {
        if target_entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as(
            r#"
            WITH requested AS (
                SELECT *
                FROM unnest($1::uuid[], $2::uuid[], $3::uuid[])
                    AS target(entry_id, publication_id, sense_id)
            )
            SELECT entry.id, publication.id, target.sense_id,
                   publication.source_revision
            FROM requested target
            JOIN lexicon.entries entry
              ON entry.id = target.entry_id
             AND entry.archived_at IS NULL
            JOIN lexicon.entry_publications publication
              ON publication.id = target.publication_id
             AND publication.entry_id = entry.id
            JOIN lexicon.entry_publication_nodes sense
              ON sense.publication_id = publication.id
             AND sense.entry_id = entry.id
             AND sense.node_id = target.sense_id
             AND sense.node_type = 'sense'
            ORDER BY entry.id, publication.id, target.sense_id
            FOR SHARE OF entry NOWAIT
            "#,
        )
        .bind(target_entry_ids)
        .bind(target_publication_ids)
        .bind(target_sense_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_target_publication_lock_error)
    }
}
