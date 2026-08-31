use super::*;
use serde_json::Value;

// --- 例句关联 ---

impl LexiconRepository {
    pub(crate) async fn sentence_has_segmented_associations(
        tx: &mut Transaction<'_, Postgres>,
        sentence_id: Uuid,
    ) -> Result<bool, LexiconRepositoryError> {
        sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM lexicon.sentence_associations
                WHERE sentence_id = $1
                  AND segment_count > 1
            )
            "#,
        )
        .bind(sentence_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn sentence_associations<'e, E>(
        executor: E,
        entry_id: Uuid,
    ) -> Result<Vec<SentenceAssociationRecord>, LexiconRepositoryError>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        sqlx::query_as::<_, SentenceAssociationRecord>(
            r#"
            SELECT association.id, association.entry_id, association.sentence_id, association.source_dialect,
                   association.association_schema_version, association.segment_count,
                   association.segments_fingerprint,
                   COALESCE((
                       SELECT jsonb_agg(
                           jsonb_build_object(
                               'start', segment.range_start,
                               'end', segment.range_end,
                               'surface', segment.surface
                           )
                           ORDER BY segment.ordinal
                       )
                       FROM lexicon.sentence_association_segments segment
                       WHERE segment.association_id = association.id
                   ), '[]'::jsonb) AS source_segments,
                   association.range_start, association.range_end, association.surface,
                   association.state, association.target_entry_id, association.target_sense_id,
                   association.target_form_slot_id, association.target_publication_id,
                   association.target_form_variant_id,
                   association.target_component_usages_snapshot, association.origin,
                   association.target_headword_snapshot, association.target_gloss_snapshot,
                   association.resolved_pos, association.resolved_form_type,
                   association.pending_target_kind, association.pending_target_headword,
                   association.normalized_pending_target_headword,
                   association.pending_target_gloss
            FROM lexicon.sentence_associations association
            WHERE association.entry_id = $1
            ORDER BY association.sentence_id, association.source_dialect, association.range_start
            "#,
        )
        .bind(entry_id)
        .fetch_all(executor)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn sentence_association_scans<'e, E>(
        executor: E,
        entry_id: Uuid,
    ) -> Result<Vec<SentenceAssociationScanRecord>, LexiconRepositoryError>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        sqlx::query_as::<_, SentenceAssociationScanRecord>(
            r#"
            SELECT sentence_id, source_dialect, text_hash, resolver_version
            FROM lexicon.sentence_association_scans
            WHERE entry_id = $1
            "#,
        )
        .bind(entry_id)
        .fetch_all(executor)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn pending_sentence_associations_for_target(
        pool: &PgPool,
        target_entry_id: Uuid,
        cursor: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<PendingSentenceAssociationListRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, PendingSentenceAssociationListRecord>(
            r#"
            WITH target AS (
                SELECT id, kind, current_publication_id
                FROM lexicon.entries
                WHERE id = $1
                  AND archived_at IS NULL
                  AND current_publication_id IS NOT NULL
            ), target_surfaces AS (
                SELECT DISTINCT source.normalized_surface, target.kind
                FROM target
                JOIN lexicon.surface_sources source
                  ON source.entry_id = target.id
                 AND source.publication_id = target.current_publication_id
                 AND source.content_scope = 'current_publication'
                 AND source.is_deleted = FALSE
                 AND source.language = 'en'
            )
            SELECT association.id,
                   association.entry_id,
                   owner.revision AS owner_revision,
                   owner.lifecycle_revision AS owner_lifecycle_revision,
                   association.sentence_id,
                   association.source_dialect,
                   association.association_schema_version,
                   association.segment_count,
                   COALESCE((
                       SELECT jsonb_agg(
                           jsonb_build_object(
                               'start', segment.range_start,
                               'end', segment.range_end,
                               'surface', segment.surface
                           )
                           ORDER BY segment.ordinal
                       )
                       FROM lexicon.sentence_association_segments segment
                       WHERE segment.association_id = association.id
                   ), '[]'::jsonb) AS source_segments,
                   text.plain_text AS sentence_text,
                   association.pending_target_kind,
                   association.pending_target_headword,
                   association.pending_target_gloss,
                   scan.text_hash AS scan_text_hash,
                   scan.resolver_version AS scan_resolver_version
            FROM lexicon.sentence_associations association
            JOIN target_surfaces target
              ON target.normalized_surface = association.normalized_pending_target_headword
             AND target.kind = association.pending_target_kind
            JOIN lexicon.entries owner
              ON owner.id = association.entry_id
             AND owner.archived_at IS NULL
             AND owner.content_schema_version = 3
            JOIN lexicon.sentence_association_scans scan
              ON scan.entry_id = association.entry_id
             AND scan.sentence_id = association.sentence_id
             AND scan.source_dialect = association.source_dialect
            JOIN lexicon.text_variants text
              ON text.entry_id = association.entry_id
             AND text.owner_node_id = association.sentence_id
             AND text.field_role = 'en_text'
             AND text.language = 'en'
             AND text.dialect = association.source_dialect
            WHERE association.state = 'pending'
              AND ($2::uuid IS NULL OR association.id > $2)
            ORDER BY association.id
            LIMIT $3
            "#,
        )
        .bind(target_entry_id)
        .bind(cursor)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn pending_sentence_association_count_for_target(
        pool: &PgPool,
        target_entry_id: Uuid,
        resolver_version: i16,
    ) -> Result<i64, LexiconRepositoryError> {
        sqlx::query_scalar(
            r#"
            WITH target AS (
                SELECT id, kind, current_publication_id
                FROM lexicon.entries
                WHERE id = $1
                  AND archived_at IS NULL
                  AND current_publication_id IS NOT NULL
            ), target_surfaces AS (
                SELECT DISTINCT source.normalized_surface, target.kind
                FROM target
                JOIN lexicon.surface_sources source
                  ON source.entry_id = target.id
                 AND source.publication_id = target.current_publication_id
                 AND source.content_scope = 'current_publication'
                 AND source.is_deleted = FALSE
                 AND source.language = 'en'
            )
            SELECT COUNT(DISTINCT association.id)
            FROM lexicon.sentence_associations association
            JOIN target_surfaces target
              ON target.normalized_surface = association.normalized_pending_target_headword
             AND target.kind = association.pending_target_kind
            JOIN lexicon.entries owner
              ON owner.id = association.entry_id
             AND owner.archived_at IS NULL
             AND owner.content_schema_version = 3
            JOIN lexicon.sentence_association_scans scan
              ON scan.entry_id = association.entry_id
             AND scan.sentence_id = association.sentence_id
             AND scan.source_dialect = association.source_dialect
             AND scan.resolver_version = $2
            JOIN lexicon.text_variants text
              ON text.entry_id = association.entry_id
             AND text.owner_node_id = association.sentence_id
             AND text.field_role = 'en_text'
             AND text.language = 'en'
             AND text.dialect = association.source_dialect
             AND scan.text_hash = sha256(convert_to(text.plain_text, 'UTF8'))
            WHERE association.state = 'pending'
            "#,
        )
        .bind(target_entry_id)
        .bind(resolver_version)
        .fetch_one(pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn published_sentence_association_target_kind(
        pool: &PgPool,
        target_entry_id: Uuid,
    ) -> Result<Option<String>, LexiconRepositoryError> {
        sqlx::query_scalar(
            r#"
            SELECT kind
            FROM lexicon.entries
            WHERE id = $1
              AND archived_at IS NULL
              AND current_publication_id IS NOT NULL
            "#,
        )
        .bind(target_entry_id)
        .fetch_optional(pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn sentence_association_by_id_for_update(
        tx: &mut Transaction<'_, Postgres>,
        association_id: Uuid,
    ) -> Result<Option<SentenceAssociationRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, SentenceAssociationRecord>(
            r#"
            SELECT association.id, association.entry_id, association.sentence_id, association.source_dialect,
                   association.association_schema_version, association.segment_count,
                   association.segments_fingerprint,
                   COALESCE((
                       SELECT jsonb_agg(
                           jsonb_build_object(
                               'start', segment.range_start,
                               'end', segment.range_end,
                               'surface', segment.surface
                           )
                           ORDER BY segment.ordinal
                       )
                       FROM lexicon.sentence_association_segments segment
                       WHERE segment.association_id = association.id
                   ), '[]'::jsonb) AS source_segments,
                   association.range_start, association.range_end, association.surface,
                   association.state, association.target_entry_id, association.target_sense_id,
                   association.target_form_slot_id, association.target_publication_id,
                   association.target_form_variant_id,
                   association.target_component_usages_snapshot, association.origin,
                   association.target_headword_snapshot, association.target_gloss_snapshot,
                   association.resolved_pos, association.resolved_form_type,
                   association.pending_target_kind, association.pending_target_headword,
                   association.normalized_pending_target_headword,
                   association.pending_target_gloss
            FROM lexicon.sentence_associations association
            WHERE association.id = $1
            FOR UPDATE
            "#,
        )
        .bind(association_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn sentence_association_owner_id(
        tx: &mut Transaction<'_, Postgres>,
        association_id: Uuid,
    ) -> Result<Option<Uuid>, LexiconRepositoryError> {
        sqlx::query_scalar("SELECT entry_id FROM lexicon.sentence_associations WHERE id = $1")
            .bind(association_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn sentence_association_current_text(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        sentence_id: Uuid,
        source_dialect: &str,
    ) -> Result<Option<String>, LexiconRepositoryError> {
        sqlx::query_scalar(
            r#"
            SELECT plain_text
            FROM lexicon.text_variants
            WHERE entry_id = $1
              AND owner_node_id = $2
              AND field_role = 'en_text'
              AND language = 'en'
              AND dialect = $3
            "#,
        )
        .bind(entry_id)
        .bind(sentence_id)
        .bind(source_dialect)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn claim_pending_sentence_association(
        tx: &mut Transaction<'_, Postgres>,
        association_id: Uuid,
        target_entry_id: Uuid,
        target_sense_id: Uuid,
        target_form_slot_id: Option<Uuid>,
        target_publication_id: Option<Uuid>,
        target_form_variant_id: Option<Uuid>,
        target_component_usages_snapshot: Option<&Value>,
        target_headword_snapshot: &str,
        target_gloss_snapshot: &str,
        resolved_pos: &str,
        resolved_form_type: Option<&str>,
    ) -> Result<(), LexiconRepositoryError> {
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.sentence_associations
            SET state = 'linked',
                target_entry_id = $2,
                target_sense_id = $3,
                target_form_slot_id = $4,
                target_publication_id = $5,
                target_form_variant_id = $6,
                target_component_usages_snapshot = $7,
                target_headword_snapshot = $8,
                target_gloss_snapshot = $9,
                resolved_pos = $10,
                resolved_form_type = $11,
                pending_target_kind = NULL,
                pending_target_headword = NULL,
                normalized_pending_target_headword = NULL,
                pending_target_gloss = NULL,
                updated_at = now()
            WHERE id = $1 AND state = 'pending'
            "#,
        )
        .bind(association_id)
        .bind(target_entry_id)
        .bind(target_sense_id)
        .bind(target_form_slot_id)
        .bind(target_publication_id)
        .bind(target_form_variant_id)
        .bind(target_component_usages_snapshot)
        .bind(target_headword_snapshot)
        .bind(target_gloss_snapshot)
        .bind(resolved_pos)
        .bind(resolved_form_type)
        .execute(&mut **tx)
        .await
        .map_err(map_sentence_association_write_error)?;
        if updated.rows_affected() != 1 {
            return Err(LexiconRepositoryError::Invariant(
                "locked pending sentence association changed before claim",
            ));
        }
        Ok(())
    }

    /// 整体替换某条例句某一侧的关联，并记下这一侧正文解析到哪个版本。
    ///
    /// 删后插而不是逐条比对：一侧的关联最多十来条，位置一改主键就变，
    /// 比对的复杂度换不来任何东西。
    pub(crate) async fn replace_sentence_associations(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        sentence_id: Uuid,
        source_dialect: &str,
        associations: &[NewSentenceAssociation],
        scan: Option<(&[u8], i16)>,
    ) -> Result<(), LexiconRepositoryError> {
        sqlx::query(
            "DELETE FROM lexicon.sentence_associations WHERE sentence_id = $1 AND source_dialect = $2",
        )
        .bind(sentence_id)
        .bind(source_dialect)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        for association in associations {
            sqlx::query(
                r#"
                INSERT INTO lexicon.sentence_associations (
                    id, entry_id, sentence_id, source_dialect,
                    association_schema_version, segment_count, segments_fingerprint,
                    range_start, range_end, surface,
                    state, target_entry_id, target_sense_id, target_form_slot_id,
                    target_publication_id, target_form_variant_id,
                    target_component_usages_snapshot, origin,
                    target_headword_snapshot, target_gloss_snapshot, resolved_pos, resolved_form_type,
                    pending_target_kind, pending_target_headword,
                    normalized_pending_target_headword, pending_target_gloss
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13, $14, $15, $16, $17, $18, $19, $20,
                    $21, $22, $23, $24, $25, $26
                )
                "#,
            )
            .bind(association.id)
            .bind(entry_id)
            .bind(association.sentence_id)
            .bind(&association.source_dialect)
            .bind(association.association_schema_version)
            .bind(i16::try_from(association.source_segments.len()).map_err(|_| {
                LexiconRepositoryError::Invariant("sentence association segment count overflow")
            })?)
            .bind(association.segments_fingerprint.as_deref())
            .bind(association.range_start)
            .bind(association.range_end)
            .bind(&association.surface)
            .bind(&association.state)
            .bind(association.target_entry_id)
            .bind(association.target_sense_id)
            .bind(association.target_form_slot_id)
            .bind(association.target_publication_id)
            .bind(association.target_form_variant_id)
            .bind(association.target_component_usages_snapshot.as_ref())
            .bind(&association.origin)
            .bind(association.target_headword_snapshot.as_deref())
            .bind(association.target_gloss_snapshot.as_deref())
            .bind(association.resolved_pos.as_deref())
            .bind(association.resolved_form_type.as_deref())
            .bind(association.pending_target_kind.as_deref())
            .bind(association.pending_target_headword.as_deref())
            .bind(
                association
                    .normalized_pending_target_headword
                    .as_deref(),
            )
            .bind(association.pending_target_gloss.as_deref())
            .execute(&mut **tx)
            .await
            .map_err(map_sentence_association_write_error)?;

            if association.association_schema_version == 3 {
                for (ordinal, segment) in association.source_segments.iter().enumerate() {
                    sqlx::query(
                        r#"
                        INSERT INTO lexicon.sentence_association_segments (
                            association_id, ordinal, sentence_id, source_dialect,
                            range_start, range_end, surface
                        ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                        "#,
                    )
                    .bind(association.id)
                    .bind(i16::try_from(ordinal).map_err(|_| {
                        LexiconRepositoryError::Invariant(
                            "sentence association segment ordinal overflow",
                        )
                    })?)
                    .bind(association.sentence_id)
                    .bind(&association.source_dialect)
                    .bind(segment.range_start)
                    .bind(segment.range_end)
                    .bind(&segment.surface)
                    .execute(&mut **tx)
                    .await
                    .map_err(map_sentence_association_write_error)?;
                }
            }
        }

        if let Some((text_hash, resolver_version)) = scan {
            sqlx::query(
                r#"
                INSERT INTO lexicon.sentence_association_scans (
                    sentence_id, entry_id, source_dialect, text_hash, resolver_version, scanned_at
                ) VALUES ($1, $2, $3, $4, $5, now())
                ON CONFLICT (sentence_id, source_dialect) DO UPDATE
                SET text_hash = EXCLUDED.text_hash,
                    resolver_version = EXCLUDED.resolver_version,
                    scanned_at = EXCLUDED.scanned_at
                "#,
            )
            .bind(sentence_id)
            .bind(entry_id)
            .bind(source_dialect)
            .bind(text_hash)
            .bind(resolver_version)
            .execute(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)?;
        }
        Ok(())
    }

    /// 删掉本词条下已经不存在的「例句 × 方言侧」——例句被删掉，或 en_text 从
    /// distinguish 改回了 unified。
    pub(crate) async fn prune_sentence_associations(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        live_sentence_ids: &[Uuid],
        live_dialects: &[String],
    ) -> Result<(), LexiconRepositoryError> {
        for statement in [
            r#"
            DELETE FROM lexicon.sentence_associations stale
            WHERE stale.entry_id = $1
              AND NOT EXISTS (
                  SELECT 1
                  FROM unnest($2::uuid[], $3::text[]) AS live(sentence_id, source_dialect)
                  WHERE live.sentence_id = stale.sentence_id
                    AND live.source_dialect = stale.source_dialect
              )
            "#,
            r#"
            DELETE FROM lexicon.sentence_association_scans stale
            WHERE stale.entry_id = $1
              AND NOT EXISTS (
                  SELECT 1
                  FROM unnest($2::uuid[], $3::text[]) AS live(sentence_id, source_dialect)
                  WHERE live.sentence_id = stale.sentence_id
                    AND live.source_dialect = stale.source_dialect
              )
            "#,
        ] {
            sqlx::query(statement)
                .bind(entry_id)
                .bind(live_sentence_ids)
                .bind(live_dialects)
                .execute(&mut **tx)
                .await
                .map_err(LexiconRepositoryError::Database)?;
        }
        Ok(())
    }

    /// 事后修正关联只推进 `lifecycle_revision`：改的是已发布内容的附属数据，
    /// 推进 `revision` 会把词条判成「有未发布改动」，逼出一次没必要的重新发布。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn record_sentence_association_edit(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        revision: i64,
        lifecycle_revision: i64,
        updated_at: chrono::DateTime<chrono::Utc>,
        actor_id: Uuid,
        request_id: Uuid,
        sentence_id: Uuid,
        audit_action: &'static str,
    ) -> Result<(), LexiconRepositoryError> {
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.entries
            SET lifecycle_revision = $2,
                updated_by_admin_id = $3,
                updated_at = $4
            WHERE id = $1 AND lifecycle_revision = $5 AND revision = $6
            "#,
        )
        .bind(entry_id)
        .bind(lifecycle_revision)
        .bind(actor_id)
        .bind(updated_at)
        .bind(lifecycle_revision - 1)
        .bind(revision)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        if updated.rows_affected() != 1 {
            return Err(LexiconRepositoryError::Invariant(
                "locked entry lifecycle revision changed during association edit",
            ));
        }
        insert_audit_action(
            tx,
            actor_id,
            audit_action,
            entry_id,
            revision,
            request_id,
            serde_json::json!({
                "sentence_id": sentence_id,
                "lifecycle_revision": lifecycle_revision,
            }),
        )
        .await
    }

    /// 自动关联的候选词形：只认未归档词条**当前发布版本**里真实录入的词形。
    ///
    /// 只查各 schema 的真实词形行（V2 `form` / V3 `form_variant`）——headword 行的
    /// `pos` / `form_type` 是 NULL，而这两列正是筛选口径与只读投影的依据。
    ///
    /// 刻意不加行锁：自动关联不登记 publication 引用，目标变了也只是快照旧一点，
    /// 为它把几十个无关词条锁进发布事务只会凭空造出 `reference_conflict`。
    pub(crate) async fn published_form_surfaces(
        tx: &mut Transaction<'_, Postgres>,
        source_entry_id: Uuid,
        dialect_scopes: &[String],
        normalized_surfaces: &[String],
        allow_v3: bool,
    ) -> Result<Vec<PublishedFormSurfaceRecord>, LexiconRepositoryError> {
        if normalized_surfaces.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, PublishedFormSurfaceRecord>(
            r#"
            SELECT DISTINCT
                   source.normalized_surface,
                   source.dialect_scope,
                   source.entry_id,
                   source.source_node_id,
                   source.pos_id,
                   source.pos
            FROM lexicon.surface_sources source
            JOIN lexicon.entries entry
              ON entry.id = source.entry_id
             AND entry.archived_at IS NULL
             AND entry.current_publication_id = source.publication_id
            WHERE source.is_deleted = FALSE
              AND source.content_scope = 'current_publication'
              AND source.language = 'en'
              AND source.entry_kind = 'word'
              AND source.source_kind = ANY($5::text[])
              AND source.normalization_version = $1
              AND source.entry_id <> $2
              AND source.dialect_scope = ANY($3::text[])
              AND source.normalized_surface = ANY($4::text[])
            "#,
        )
        .bind(HEADWORD_NORMALIZATION_VERSION)
        .bind(source_entry_id)
        .bind(dialect_scopes)
        .bind(normalized_surfaces)
        .bind(association_form_source_kinds(allow_v3))
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// 候选词条的当前发布快照——词义、词形槽位、词头与释义都从这里取。
    pub(crate) async fn current_publication_snapshots(
        tx: &mut Transaction<'_, Postgres>,
        entry_ids: &[Uuid],
    ) -> Result<Vec<PublishedEntrySnapshotRecord>, LexiconRepositoryError> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, PublishedEntrySnapshotRecord>(
            r#"
            SELECT entry.id AS entry_id, publication.id AS publication_id, publication.snapshot
            FROM lexicon.entries entry
            JOIN lexicon.entry_publications publication
              ON publication.id = entry.current_publication_id
             AND publication.entry_id = entry.id
            WHERE entry.id = ANY($1::uuid[])
              AND entry.archived_at IS NULL
            "#,
        )
        .bind(entry_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn historical_publication_snapshots(
        tx: &mut Transaction<'_, Postgres>,
        targets: &[(Uuid, Uuid)],
    ) -> Result<Vec<PublishedEntrySnapshotRecord>, LexiconRepositoryError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let entry_ids = targets
            .iter()
            .map(|(entry_id, _)| *entry_id)
            .collect::<Vec<_>>();
        let publication_ids = targets
            .iter()
            .map(|(_, publication_id)| *publication_id)
            .collect::<Vec<_>>();
        sqlx::query_as::<_, PublishedEntrySnapshotRecord>(
            r#"
            WITH requested AS (
                SELECT *
                FROM unnest($1::uuid[], $2::uuid[])
                    AS target(entry_id, publication_id)
            )
            SELECT entry.id AS entry_id, publication.id AS publication_id,
                   publication.snapshot
            FROM requested target
            JOIN lexicon.entries entry
              ON entry.id = target.entry_id
             AND entry.archived_at IS NULL
            JOIN lexicon.entry_publications publication
              ON publication.id = target.publication_id
             AND publication.entry_id = entry.id
            ORDER BY entry.id, publication.id
            "#,
        )
        .bind(entry_ids)
        .bind(publication_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }
}

/// 目标词义/词形槽位在写入的一瞬间被别的事务删掉时，外键会先报出来。
/// 对自动关联来说这不是错误，只是那条关联建不成——但它发生在整批插入中间，
/// 交给调用方按「目标不可用」处理比让整个发布挂掉合理。
fn map_sentence_association_write_error(error: sqlx::Error) -> LexiconRepositoryError {
    if is_foreign_key_violation(&error, "lexicon_sentence_associations_target_fkey")
        || is_foreign_key_violation(&error, "lexicon_sentence_associations_target_slot_fkey")
        || is_foreign_key_violation(
            &error,
            "lexicon_sentence_associations_target_publication_fkey",
        )
        || is_foreign_key_violation(&error, "lexicon_sentence_associations_target_variant_fkey")
        || is_foreign_key_violation(
            &error,
            "lexicon_sentence_associations_target_publication_variant_fkey",
        )
        || is_foreign_key_violation(
            &error,
            "lexicon_sentence_associations_target_publication_sense_fkey",
        )
        || is_foreign_key_violation(
            &error,
            "lexicon_sentence_associations_target_publication_form_fkey",
        )
    {
        return LexiconRepositoryError::ReferenceTargetChanged;
    }
    LexiconRepositoryError::Database(error)
}
