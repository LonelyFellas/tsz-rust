use super::*;

impl LexiconRepository {
    pub(crate) async fn related_search_dataset_version(
        &self,
    ) -> Result<i64, LexiconRepositoryError> {
        // aggregate_type 是 outbox 唯一索引的首列；这里只扫描词库发布/生命周期索引区间，
        // 草稿保存与其他业务事件不会让已签名游标无故失效。
        sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM platform.outbox_events
            WHERE aggregate_type IN ('lexicon.entry', 'lexicon.entry.lifecycle')
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn related_search(
        &self,
        filter: &RelatedSearchFilter<'_>,
    ) -> Result<Vec<RelatedSearchRecord>, LexiconRepositoryError> {
        let pattern = format!("%{}%", escape_like_literal(filter.q));
        sqlx::query_as::<_, RelatedSearchRecord>(
            r#"
            WITH published_entry AS (
                SELECT entry.id,
                       entry.kind,
                       publication.id AS publication_id,
                       publication.snapshot,
                       CASE publication.snapshot #>> '{headwords,mode}'
                           WHEN 'unified' THEN
                               COALESCE(publication.snapshot #>> '{headwords,common}', '')
                           WHEN 'distinguish' THEN concat_ws(
                               ' / ',
                               NULLIF(publication.snapshot #>> '{headwords,uk}', ''),
                               NULLIF(publication.snapshot #>> '{headwords,us}', '')
                           )
                           ELSE ''
                       END AS headword
                FROM lexicon.entries entry
                JOIN lexicon.entry_publications publication
                  ON publication.id = entry.current_publication_id
                 AND publication.entry_id = entry.id
                WHERE entry.archived_at IS NULL
            ), searchable_entry AS (
                SELECT *, headword AS sort_headword
                FROM published_entry
            )
            SELECT snapshot,
                   COALESCE((
                       SELECT array_agg(DISTINCT part.code ORDER BY part.code)
                       FROM lexicon.entry_publication_part_of_speech_refs pos_ref
                       JOIN catalog.parts_of_speech part
                         ON part.id = pos_ref.part_of_speech_id
                       WHERE pos_ref.publication_id = searchable_entry.publication_id
                         AND pos_ref.entry_id = searchable_entry.id
                   ), ARRAY[]::text[]) AS pos_labels,
                   sort_headword,
                   count(*) OVER() AS total
            FROM searchable_entry
            WHERE ($2::text IS NULL OR kind = $2)
              AND CASE WHEN $3 THEN
                    EXISTS (
                        SELECT 1
                        FROM lexicon.surface_sources surface
                        WHERE surface.entry_id = searchable_entry.id
                          AND surface.publication_id = searchable_entry.publication_id
                          AND surface.content_scope = 'current_publication'
                          AND surface.source_kind = 'headword'
                          AND surface.is_deleted = FALSE
                          AND surface.normalized_surface = $5
                    )
                  ELSE
                    EXISTS (
                        SELECT 1
                        FROM lexicon.surface_sources surface
                        WHERE surface.entry_id = searchable_entry.id
                          AND surface.publication_id = searchable_entry.publication_id
                          AND surface.content_scope = 'current_publication'
                          AND surface.source_kind = 'headword'
                          AND surface.is_deleted = FALSE
                          AND surface.normalized_surface LIKE $1 ESCAPE '\'
                    )
                  END
              AND (NOT $4 OR NOT (
                    EXISTS (
                        SELECT 1
                        FROM lexicon.surface_sources surface
                        WHERE surface.entry_id = searchable_entry.id
                          AND surface.publication_id = searchable_entry.publication_id
                          AND surface.content_scope = 'current_publication'
                          AND surface.source_kind = 'headword'
                          AND surface.is_deleted = FALSE
                          AND surface.normalized_surface = $5
                    )
                  ))
              AND ($6::text IS NULL OR (
                    kind,
                    sort_headword COLLATE "C",
                    id
                  ) > ($6, $7 COLLATE "C", $8)
              )
            ORDER BY kind ASC, sort_headword COLLATE "C" ASC, id ASC
            LIMIT $9
            "#,
        )
        .bind(pattern)
        .bind(filter.kind.map(kind_string))
        .bind(filter.exact)
        .bind(filter.exclude_exact)
        .bind(filter.q)
        .bind(filter.last_kind.map(kind_string))
        .bind(filter.last_headword)
        .bind(filter.last_word_id)
        .bind(filter.limit)
        .fetch_all(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn list(
        &self,
        filter: &ListFilter,
    ) -> Result<Vec<ListEntryRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, ListEntryRecord>(
            r#"
            SELECT entry.id,
                   entry.kind,
                   entry.source_dialect,
                   -- 并列拼写一律「检测基准侧在前」，与词条详情、建稿第 4 步保持一致；
                   -- source_dialect 为 NULL（unified）时回落到 common → uk → us。
                   COALESCE((
                       SELECT array_agg(headword.dialect ORDER BY CASE
                           WHEN headword.dialect = 'common' THEN 0
                           WHEN headword.dialect = entry.source_dialect THEN 1
                           WHEN headword.dialect = 'uk' THEN 2
                           ELSE 3 END)
                       FROM lexicon.entry_headwords headword
                       WHERE headword.entry_id = entry.id
                   ), ARRAY[]::text[]) AS dialects,
                   entry.revision,
                   entry.lifecycle_revision,
                   COALESCE((
                       SELECT string_agg(headword.headword, ' / ' ORDER BY CASE
                           WHEN headword.dialect = 'common' THEN 0
                           WHEN headword.dialect = entry.source_dialect THEN 1
                           WHEN headword.dialect = 'uk' THEN 2
                           ELSE 3 END)
                       FROM lexicon.entry_headwords headword
                       WHERE headword.entry_id = entry.id
                   ), '') AS headword,
                   COALESCE((
                       SELECT variant.plain_text
                       FROM lexicon.definitions definition
                       JOIN lexicon.text_variants variant ON variant.owner_node_id = definition.id
                       WHERE definition.entry_id = entry.id
                         AND definition.language = 'zh'
                         AND variant.field_role = 'content'
                       ORDER BY definition.sort_order, definition.id, variant.sort_order, variant.id
                       LIMIT 1
                   ), '') AS gloss,
                   COALESCE((
                       SELECT array_agg(part.code ORDER BY pos.sort_order, pos.id)
                       FROM lexicon.entry_pos pos
                       JOIN catalog.parts_of_speech part ON part.id = pos.part_of_speech_id
                       WHERE pos.entry_id = entry.id
                   ), ARRAY[]::text[]) AS pos_list,
                   COALESCE((
                       SELECT array_agg(DISTINCT sense.level ORDER BY sense.level)
                       FROM lexicon.senses sense
                       WHERE sense.entry_id = entry.id
                   ), ARRAY[]::text[]) AS levels,
                   entry.current_publication_id IS NOT NULL AS is_published,
                   publication.source_revision AS published_revision,
                   publication.source_revision IS NOT NULL
                       AND publication.source_revision <> entry.revision
                       AS has_unpublished_changes,
                   entry.archived_at IS NOT NULL AS is_archived,
                   COALESCE((
                       SELECT array_agg(progress.step ORDER BY CASE progress.step
                           WHEN 'basics' THEN 1 WHEN 'forms' THEN 2 WHEN 'meanings' THEN 3 END)
                       FROM lexicon.entry_step_progress progress
                       WHERE progress.entry_id = entry.id
                   ), ARRAY[]::text[]) AS completed_steps,
                   creator.display_name AS created_by_name,
                   entry.created_at,
                   entry.updated_at,
                   count(*) OVER() AS total
            FROM lexicon.entries entry
            JOIN admins creator ON creator.id = entry.created_by_admin_id
            LEFT JOIN lexicon.entry_publications publication
                ON publication.id = entry.current_publication_id
            WHERE (
                    ($6::text IS NULL AND entry.archived_at IS NULL)
                    OR ($6 = 'draft' AND entry.archived_at IS NULL AND entry.current_publication_id IS NULL)
                    OR ($6 = 'published' AND entry.archived_at IS NULL AND entry.current_publication_id IS NOT NULL)
                    OR ($6 = 'archived' AND entry.archived_at IS NOT NULL)
                  )
              AND ($1::text IS NULL OR creator.display_name ILIKE '%' || $1 || '%'
                   OR EXISTS (
                       SELECT 1 FROM lexicon.entry_headwords h
                       WHERE h.entry_id = entry.id AND h.headword ILIKE '%' || $1 || '%'
                   ))
              AND ($2::text IS NULL OR EXISTS (
                   SELECT 1 FROM lexicon.text_variants v
                   JOIN lexicon.definitions d ON d.id = v.owner_node_id
                   WHERE d.entry_id = entry.id AND v.plain_text ILIKE '%' || $2 || '%'
              ))
              AND ($3::text IS NULL OR entry.kind = $3)
              AND ($4::text IS NULL OR EXISTS (
                   SELECT 1 FROM lexicon.entry_pos p
                   JOIN catalog.parts_of_speech c ON c.id = p.part_of_speech_id
                   WHERE p.entry_id = entry.id AND c.code = $4
              ))
              AND ($5::text IS NULL OR EXISTS (
                   SELECT 1 FROM lexicon.senses s WHERE s.entry_id = entry.id AND s.level = $5
              ))
              AND ($7::timestamptz IS NULL OR entry.created_at >= $7)
              AND ($8::timestamptz IS NULL OR entry.created_at < $8)
            ORDER BY entry.created_at DESC, entry.id DESC
            LIMIT $9 OFFSET $10
            "#,
        )
        .bind(filter.q.as_deref())
        .bind(filter.gloss.as_deref())
        .bind(filter.kind.as_deref())
        .bind(filter.pos.as_deref())
        .bind(filter.level.as_deref())
        .bind(filter.status.as_deref())
        .bind(filter.created_from)
        .bind(filter.created_to)
        .bind(filter.limit)
        .bind(filter.offset)
        .fetch_all(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn list_total(
        &self,
        filter: &ListFilter,
    ) -> Result<i64, LexiconRepositoryError> {
        sqlx::query_scalar(
            r#"
            SELECT count(*)::bigint
            FROM lexicon.entries entry
            JOIN admins creator ON creator.id = entry.created_by_admin_id
            WHERE (
                    ($6::text IS NULL AND entry.archived_at IS NULL)
                    OR ($6 = 'draft' AND entry.archived_at IS NULL AND entry.current_publication_id IS NULL)
                    OR ($6 = 'published' AND entry.archived_at IS NULL AND entry.current_publication_id IS NOT NULL)
                    OR ($6 = 'archived' AND entry.archived_at IS NOT NULL)
                  )
              AND ($1::text IS NULL OR creator.display_name ILIKE '%' || $1 || '%'
                   OR EXISTS (
                       SELECT 1 FROM lexicon.entry_headwords h
                       WHERE h.entry_id = entry.id AND h.headword ILIKE '%' || $1 || '%'
                   ))
              AND ($2::text IS NULL OR EXISTS (
                   SELECT 1 FROM lexicon.text_variants v
                   JOIN lexicon.definitions d ON d.id = v.owner_node_id
                   WHERE d.entry_id = entry.id AND v.plain_text ILIKE '%' || $2 || '%'
              ))
              AND ($3::text IS NULL OR entry.kind = $3)
              AND ($4::text IS NULL OR EXISTS (
                   SELECT 1 FROM lexicon.entry_pos p
                   JOIN catalog.parts_of_speech c ON c.id = p.part_of_speech_id
                   WHERE p.entry_id = entry.id AND c.code = $4
              ))
              AND ($5::text IS NULL OR EXISTS (
                   SELECT 1 FROM lexicon.senses s WHERE s.entry_id = entry.id AND s.level = $5
              ))
              AND ($7::timestamptz IS NULL OR entry.created_at >= $7)
              AND ($8::timestamptz IS NULL OR entry.created_at < $8)
            "#,
        )
        .bind(filter.q.as_deref())
        .bind(filter.gloss.as_deref())
        .bind(filter.kind.as_deref())
        .bind(filter.pos.as_deref())
        .bind(filter.level.as_deref())
        .bind(filter.status.as_deref())
        .bind(filter.created_from)
        .bind(filter.created_to)
        .fetch_one(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn stats(&self) -> Result<StatsRecord, LexiconRepositoryError> {
        sqlx::query_as::<_, StatsRecord>(
            r#"
            SELECT count(*)::bigint AS total,
                   count(*) FILTER (
                       WHERE (created_at AT TIME ZONE 'Asia/Shanghai')::date =
                             (now() AT TIME ZONE 'Asia/Shanghai')::date
                   )::bigint AS today,
                   count(*) FILTER (
                       WHERE date_trunc('month', created_at AT TIME ZONE 'Asia/Shanghai') =
                             date_trunc('month', now() AT TIME ZONE 'Asia/Shanghai')
                   )::bigint AS month
            FROM lexicon.entries
            WHERE archived_at IS NULL
            "#,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }
}
