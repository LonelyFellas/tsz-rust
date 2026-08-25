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
                       publication.content_schema_version,
                       publication.snapshot,
                       -- 只作排序/游标键；必须与服务端展示字段逐字符相同。
                       -- V2 并列拼写仍按管理员主词侧在前；V3 没有主词，直接使用
                       -- publication snapshot 内冻结的 presentation label。
                       CASE publication.content_schema_version
                           WHEN 3 THEN COALESCE(
                               publication.snapshot #>> '{presentation,label}',
                               ''
                           )
                           ELSE CASE publication.snapshot #>> '{headwords,mode}'
                               WHEN 'unified' THEN
                                   COALESCE(publication.snapshot #>> '{headwords,common}', '')
                               WHEN 'distinguish' THEN
                                   CASE WHEN publication.snapshot #>> '{headwords,source_dialect}' = 'us'
                                       THEN concat_ws(
                                           ' / ',
                                           NULLIF(publication.snapshot #>> '{headwords,us}', ''),
                                           NULLIF(publication.snapshot #>> '{headwords,uk}', '')
                                       )
                                       ELSE concat_ws(
                                           ' / ',
                                           NULLIF(publication.snapshot #>> '{headwords,uk}', ''),
                                           NULLIF(publication.snapshot #>> '{headwords,us}', '')
                                       )
                                   END
                               ELSE ''
                           END
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
            SELECT content_schema_version,
                   snapshot,
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
            WHERE ($10::boolean OR content_schema_version = 2)
              AND ($2::text IS NULL OR kind = $2)
              AND CASE WHEN $3 THEN
                    EXISTS (
                        SELECT 1
                        FROM lexicon.surface_sources surface
                        WHERE surface.entry_id = searchable_entry.id
                          AND surface.publication_id = searchable_entry.publication_id
                          AND surface.content_scope = 'current_publication'
                          AND surface.content_schema_version = searchable_entry.content_schema_version
                          AND (
                              (searchable_entry.content_schema_version = 2
                                  AND surface.source_kind = 'headword')
                              OR (searchable_entry.content_schema_version = 3
                                  AND surface.source_kind = 'form_variant')
                          )
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
                          AND surface.content_schema_version = searchable_entry.content_schema_version
                          AND (
                              (searchable_entry.content_schema_version = 2
                                  AND surface.source_kind = 'headword')
                              OR (searchable_entry.content_schema_version = 3
                                  AND surface.source_kind = 'form_variant')
                          )
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
                          AND surface.content_schema_version = searchable_entry.content_schema_version
                          AND (
                              (searchable_entry.content_schema_version = 2
                                  AND surface.source_kind = 'headword')
                              OR (searchable_entry.content_schema_version = 3
                                  AND surface.source_kind = 'form_variant')
                          )
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
        .bind(filter.include_v3)
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
                   entry.content_schema_version,
                   entry.kind,
                   entry.source_dialect,
                   -- 并列拼写一律「管理员主词侧在前」，与词条详情、建稿第 4 步保持一致；
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
                   -- 每侧拼写与上面的 dialects 同序；展示用的并列串由 service 按序拼接，
                   -- 免得两个聚合各写一遍排序规则、日后又各改各的。
                   COALESCE((
                       SELECT array_agg(headword.headword ORDER BY CASE
                           WHEN headword.dialect = 'common' THEN 0
                           WHEN headword.dialect = entry.source_dialect THEN 1
                           WHEN headword.dialect = 'uk' THEN 2
                           ELSE 3 END)
                       FROM lexicon.entry_headwords headword
                       WHERE headword.entry_id = entry.id
                   ), ARRAY[]::text[]) AS headword_spellings,
                   editor.forms,
                   presentation.label AS presentation_label,
                   presentation.matched_surfaces AS presentation_surfaces,
                   presentation.strategy_version AS presentation_strategy,
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
            JOIN lexicon.entry_editor_projection editor ON editor.entry_id = entry.id
            LEFT JOIN lexicon.entry_publications publication
                ON publication.id = entry.current_publication_id
            LEFT JOIN lexicon.entry_presentation_projection presentation
                ON presentation.entry_id = entry.id
               AND presentation.source_revision = entry.revision
            WHERE (
                    ($6::text IS NULL AND entry.archived_at IS NULL)
                    OR ($6 = 'draft' AND entry.archived_at IS NULL AND entry.current_publication_id IS NULL)
                    OR ($6 = 'published' AND entry.archived_at IS NULL AND entry.current_publication_id IS NOT NULL)
                    OR ($6 = 'archived' AND entry.archived_at IS NOT NULL)
                  )
              AND ($11::boolean OR entry.content_schema_version = 2)
              AND ($1::text IS NULL OR creator.display_name ILIKE '%' || $1 || '%'
                   OR EXISTS (
                       SELECT 1 FROM lexicon.entry_headwords h
                       WHERE h.entry_id = entry.id AND h.headword ILIKE '%' || $1 || '%'
                   )
                   OR EXISTS (
                       SELECT 1 FROM lexicon.surface_sources surface
                       WHERE surface.entry_id = entry.id
                         AND surface.content_schema_version = 3
                         AND surface.is_deleted = FALSE
                         AND surface.source_revision = entry.revision
                         AND surface.surface ILIKE '%' || $1 || '%'
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
        .bind(filter.include_v3)
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
              AND ($9::boolean OR entry.content_schema_version = 2)
              AND ($1::text IS NULL OR creator.display_name ILIKE '%' || $1 || '%'
                   OR EXISTS (
                       SELECT 1 FROM lexicon.entry_headwords h
                       WHERE h.entry_id = entry.id AND h.headword ILIKE '%' || $1 || '%'
                   )
                   OR EXISTS (
                       SELECT 1 FROM lexicon.surface_sources surface
                       WHERE surface.entry_id = entry.id
                         AND surface.content_schema_version = 3
                         AND surface.is_deleted = FALSE
                         AND surface.source_revision = entry.revision
                         AND surface.surface ILIKE '%' || $1 || '%'
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
        .bind(filter.include_v3)
        .fetch_one(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn stats(
        &self,
        include_v3: bool,
    ) -> Result<StatsRecord, LexiconRepositoryError> {
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
              AND ($1 OR content_schema_version = 2)
            "#,
        )
        .bind(include_v3)
        .fetch_one(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }
}
