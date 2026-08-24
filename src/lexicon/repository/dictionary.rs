use super::*;

impl LexiconRepository {
    pub(crate) async fn dictionary_term(
        &self,
        normalized: &str,
    ) -> Result<Option<DictionaryTermRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, DictionaryTermRecord>(
            r#"
            SELECT terms.term, terms.kind, terms.pos, terms.region_family,
                   datasets.source_name AS provider_name,
                   datasets.source_version AS provider_version
            FROM dictionary.active_terms terms
            JOIN dictionary.datasets datasets ON datasets.id = terms.dataset_id
            WHERE terms.normalized_term = $1
            "#,
        )
        .bind(normalized)
        .fetch_optional(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn region_surface(
        &self,
        normalized: &str,
    ) -> Result<Option<RegionSurfaceRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, RegionSurfaceRecord>(
            r#"
            SELECT normalized_term, term, region_family, targets
            FROM dictionary.active_region_surfaces
            WHERE normalized_term = $1
            "#,
        )
        .bind(normalized)
        .fetch_optional(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn region_surfaces(
        &self,
        normalized: &[String],
    ) -> Result<Vec<RegionSurfaceRecord>, LexiconRepositoryError> {
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, RegionSurfaceRecord>(
            r#"
            SELECT normalized_term, term, region_family, targets
            FROM dictionary.active_region_surfaces
            WHERE normalized_term = ANY($1)
            ORDER BY normalized_term
            "#,
        )
        .bind(normalized)
        .fetch_all(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn dictionary_candidates(
        &self,
        normalized: &[String],
    ) -> Result<Vec<DictionaryCandidateRecord>, LexiconRepositoryError> {
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, DictionaryCandidateRecord>(
            r#"
            SELECT normalized_term, term, region_family
            FROM dictionary.active_terms
            WHERE normalized_term = ANY($1)
            "#,
        )
        .bind(normalized)
        .fetch_all(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn catalog_parts(
        &self,
        codes: &[String],
    ) -> Result<Vec<CatalogPartRecord>, LexiconRepositoryError> {
        if codes.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, CatalogPartRecord>(
            r#"
            SELECT id, code
            FROM catalog.parts_of_speech
            WHERE code = ANY($1)
            ORDER BY sort_order, id
            "#,
        )
        .bind(codes)
        .fetch_all(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// Resolve catalog rows on the caller's write transaction and retain a
    /// key-share lock until commit so a concurrent catalog delete cannot turn
    /// the later lexicon FK insert into an internal error.
    pub(crate) async fn catalog_parts_for_reference(
        tx: &mut Transaction<'_, Postgres>,
        codes: &[String],
    ) -> Result<Vec<CatalogPartRecord>, LexiconRepositoryError> {
        if codes.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, CatalogPartRecord>(
            r#"
            SELECT id, code
            FROM catalog.parts_of_speech
            WHERE code = ANY($1)
            ORDER BY sort_order, id
            FOR KEY SHARE
            "#,
        )
        .bind(codes)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// Expand/catch-up parity fallback. New detection uses the non-unique
    /// surface projection first; until B4 backfill reaches parity, the legacy
    /// exact-headword index remains authoritative for a missing exact row.
    pub(crate) async fn legacy_exact_duplicates(
        &self,
        kind: EntryKind,
        normalized: &[String],
    ) -> Result<Vec<DuplicateRecord>, LexiconRepositoryError> {
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, DuplicateRecord>(
            r#"
            SELECT DISTINCT keys.entry_id,
                   headword.headword,
                   headword.dialect,
                   entry.archived_at IS NOT NULL AS is_archived,
                   entry.current_publication_id IS NOT NULL AS is_published
            FROM lexicon.entry_headword_keys keys
            JOIN lexicon.entries entry ON entry.id = keys.entry_id
            JOIN LATERAL (
                SELECT value.headword, value.dialect
                FROM lexicon.entry_headwords value
                WHERE value.entry_id = keys.entry_id
                  AND (
                      value.normalized_headword = keys.normalized_headword
                      OR value.dialect = 'common'
                  )
                ORDER BY CASE value.dialect
                             WHEN 'common' THEN 0
                             WHEN keys.dialect_scope THEN 1
                             ELSE 2
                         END
                LIMIT 1
            ) headword ON TRUE
            WHERE keys.language = 'en'
              AND keys.kind = $1
              AND keys.normalized_headword = ANY($2)
            ORDER BY keys.entry_id, headword.dialect
            "#,
        )
        .bind(kind_string(kind))
        .bind(normalized)
        .fetch_all(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// 草稿保存用：找同名词条，**不加锁**。
    ///
    /// 这条路只绑定已有词条、不建条，所以不需要防并发建重。反过来加 FOR UPDATE
    /// 会让保存 A 的事务一直占着 B 的行到提交，把 B 自己的保存挡在外面。
    ///
    /// 归档词条也要回出（带 `is_archived`），不能在这里过滤掉：它依然占着词头唯一键，
    /// 「查不到」会被调用方当成「库里没这个词」而去建同名新条，撞
    /// `lexicon_entry_headword_keys_unique_idx`。
    pub(crate) async fn find_entry_by_headword_key(
        tx: &mut Transaction<'_, Postgres>,
        kind: EntryKind,
        normalized_headword: &str,
    ) -> Result<Option<HeadwordKeyRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, HeadwordKeyRecord>(
            r#"
            SELECT keys.entry_id,
                   entry.archived_at IS NOT NULL AS is_archived
            FROM lexicon.entry_headword_keys keys
            JOIN lexicon.entries entry ON entry.id = keys.entry_id
            WHERE keys.language = 'en'
              AND keys.kind = $1
              AND keys.normalized_headword = $2
            ORDER BY keys.entry_id
            LIMIT 1
            "#,
        )
        .bind(kind_string(kind))
        .bind(normalized_headword)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// 关联词物化用：找同名词条并锁住，避免两个并发发布各建一条同名占位。
    ///
    /// 用 entry_headword_keys 而不是 surface_sources —— 它才是词头唯一性的权威
    /// （lexicon_entry_headword_keys_unique_idx 就建在它上面），投影表是可重建的派生物。
    pub(crate) async fn find_entry_by_headword_key_for_update(
        tx: &mut Transaction<'_, Postgres>,
        kind: EntryKind,
        normalized_headword: &str,
    ) -> Result<Option<HeadwordKeyRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, HeadwordKeyRecord>(
            r#"
            SELECT keys.entry_id,
                   entry.archived_at IS NOT NULL AS is_archived
            FROM lexicon.entry_headword_keys keys
            JOIN lexicon.entries entry ON entry.id = keys.entry_id
            WHERE keys.language = 'en'
              AND keys.kind = $1
              AND keys.normalized_headword = $2
            ORDER BY keys.entry_id
            LIMIT 1
            FOR UPDATE OF entry
            "#,
        )
        .bind(kind_string(kind))
        .bind(normalized_headword)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// 关联词物化用：取目标词条草稿里排在最前的义项。
    ///
    /// 目标词条可能还停在 forms 步骤、一个义项都没有，那样就没有节点可指——
    /// 返回 None，由调用方给出可操作的校验错误，而不是去改别人的草稿。
    ///
    /// 刻意不加 FOR UPDATE：绑定路径本来就不建条、无需防并发建重，而锁住别人词条的
    /// 义项行会一直占到本事务提交，把那个词条自己的保存挡在外面。义项在提交前被删的
    /// 情况由 lexicon_relations_target_fkey 兜底。
    pub(crate) async fn first_draft_sense(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
    ) -> Result<Option<Uuid>, LexiconRepositoryError> {
        sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT sense.id
            FROM lexicon.senses sense
            JOIN lexicon.nodes node
              ON node.id = sense.id
             AND node.entry_id = sense.entry_id
             AND node.removed_from_draft_at IS NULL
            WHERE sense.entry_id = $1
            ORDER BY sense.entry_pos_id, sense.sort_order, sense.id
            LIMIT 1
            "#,
        )
        .bind(entry_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn has_unprojected_legacy_exact_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        kind: EntryKind,
        normalized: &[String],
        projected_entry_ids: &[Uuid],
    ) -> Result<bool, LexiconRepositoryError> {
        if normalized.is_empty() {
            return Ok(false);
        }
        sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM lexicon.entry_headword_keys keys
                WHERE keys.language = 'en'
                  AND keys.kind = $1
                  AND keys.normalized_headword = ANY($2)
                  AND NOT (keys.entry_id = ANY($3))
            )
            "#,
        )
        .bind(kind_string(kind))
        .bind(normalized)
        .bind(projected_entry_ids)
        .fetch_one(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn catalog_sub_parts(
        &self,
    ) -> Result<Vec<CatalogSubPartRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, CatalogSubPartRecord>(
            r#"
            SELECT sub.id, sub.code, part.code AS part_code
            FROM catalog.sub_parts_of_speech sub
            JOIN catalog.parts_of_speech part ON part.id = sub.part_of_speech_id
            ORDER BY part.sort_order, sub.sort_order, sub.id
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn catalog_sub_parts_for_reference(
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<CatalogSubPartRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, CatalogSubPartRecord>(
            r#"
            SELECT sub.id, sub.code, part.code AS part_code
            FROM catalog.sub_parts_of_speech sub
            JOIN catalog.parts_of_speech part ON part.id = sub.part_of_speech_id
            ORDER BY part.sort_order, sub.sort_order, sub.id
            FOR KEY SHARE OF sub, part
            "#,
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }
}
