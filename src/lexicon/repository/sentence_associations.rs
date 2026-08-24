use super::*;

// --- 例句关联 ---

impl LexiconRepository {
    pub(crate) async fn sentence_associations<'e, E>(
        executor: E,
        entry_id: Uuid,
    ) -> Result<Vec<SentenceAssociationRecord>, LexiconRepositoryError>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        sqlx::query_as::<_, SentenceAssociationRecord>(
            r#"
            SELECT id, sentence_id, source_dialect, range_start, range_end, surface,
                   target_entry_id, target_sense_id, target_form_slot_id, origin,
                   target_headword_snapshot, target_gloss_snapshot,
                   resolved_pos, resolved_form_type
            FROM lexicon.sentence_associations
            WHERE entry_id = $1
            ORDER BY sentence_id, source_dialect, range_start
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
                    id, entry_id, sentence_id, source_dialect, range_start, range_end, surface,
                    target_entry_id, target_sense_id, target_form_slot_id, origin,
                    target_headword_snapshot, target_gloss_snapshot, resolved_pos, resolved_form_type
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
                "#,
            )
            .bind(association.id)
            .bind(entry_id)
            .bind(association.sentence_id)
            .bind(&association.source_dialect)
            .bind(association.range_start)
            .bind(association.range_end)
            .bind(&association.surface)
            .bind(association.target_entry_id)
            .bind(association.target_sense_id)
            .bind(association.target_form_slot_id)
            .bind(&association.origin)
            .bind(&association.target_headword_snapshot)
            .bind(&association.target_gloss_snapshot)
            .bind(&association.resolved_pos)
            .bind(association.resolved_form_type.as_deref())
            .execute(&mut **tx)
            .await
            .map_err(map_sentence_association_write_error)?;
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
    pub(crate) async fn record_sentence_association_edit(
        tx: &mut Transaction<'_, Postgres>,
        word: &AdminWordV2,
        actor_id: Uuid,
        request_id: Uuid,
        sentence_id: Uuid,
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
        .bind(word.id)
        .bind(word.lifecycle_revision)
        .bind(actor_id)
        .bind(word.updated_at)
        .bind(word.lifecycle_revision - 1)
        .bind(word.revision)
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
            "lexicon.sentence_associations.replace",
            word.id,
            word.revision,
            request_id,
            serde_json::json!({
                "sentence_id": sentence_id,
                "lifecycle_revision": word.lifecycle_revision,
            }),
        )
        .await
    }

    /// 自动关联的候选词形：只认未归档词条**当前发布版本**里真实录入的词形。
    ///
    /// 只查 `source_kind = 'form'`——headword 行的 `pos` / `form_type` 是 NULL
    /// （`lexicon_surface_sources_source_shape_check`），而这两列正是筛选口径与
    /// 只读投影的依据；发布必过 `validate_forms`，词头拼写一定同时以 form 行存在。
    ///
    /// 刻意不加行锁：自动关联不登记 publication 引用，目标变了也只是快照旧一点，
    /// 为它把几十个无关词条锁进发布事务只会凭空造出 `reference_conflict`。
    pub(crate) async fn published_form_surfaces(
        tx: &mut Transaction<'_, Postgres>,
        source_entry_id: Uuid,
        dialect_scopes: &[String],
        normalized_surfaces: &[String],
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
              AND source.source_kind = 'form'
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
            SELECT entry.id AS entry_id, publication.snapshot
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
}

/// 目标词义/词形槽位在写入的一瞬间被别的事务删掉时，外键会先报出来。
/// 对自动关联来说这不是错误，只是那条关联建不成——但它发生在整批插入中间，
/// 交给调用方按「目标不可用」处理比让整个发布挂掉合理。
fn map_sentence_association_write_error(error: sqlx::Error) -> LexiconRepositoryError {
    if is_foreign_key_violation(&error, "lexicon_sentence_associations_target_fkey")
        || is_foreign_key_violation(&error, "lexicon_sentence_associations_target_slot_fkey")
    {
        return LexiconRepositoryError::ReferenceTargetChanged;
    }
    LexiconRepositoryError::Database(error)
}
