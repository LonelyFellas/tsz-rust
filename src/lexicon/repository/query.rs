use super::*;

impl LexiconRepository {
    pub(crate) async fn related_search_dataset_version(
        &self,
    ) -> Result<i64, LexiconRepositoryError> {
        // 草稿候选进入数据集后，forms surface projection 与 meanings 保存事件都必须
        // 使已签名游标失效；发布/归档/恢复继续由 entry/lifecycle 事件覆盖。
        sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM platform.outbox_events
            WHERE aggregate_type IN (
                'lexicon.entry',
                'lexicon.entry.lifecycle',
                'lexicon.surface_projection'
            )
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
            WITH searchable_entry AS (
                SELECT entry.id,
                       entry.kind,
                       publication.id AS publication_id,
                       NULL::bigint AS source_revision,
                       publication.content_schema_version,
                       publication.snapshot,
                       'published'::text AS status,
                       0::smallint AS status_rank,
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
                       END AS sort_headword
                FROM lexicon.entries entry
                JOIN lexicon.entry_publications publication
                  ON publication.id = entry.current_publication_id
                 AND publication.entry_id = entry.id
                WHERE entry.archived_at IS NULL

                UNION ALL

                SELECT entry.id,
                       entry.kind,
                       NULL::uuid AS publication_id,
                       entry.revision AS source_revision,
                       3::smallint AS content_schema_version,
                       jsonb_build_object(
                           'id', entry.id,
                           'kind', entry.kind,
                           'presentation', jsonb_build_object(
                               'label', presentation.label,
                               'matched_surfaces', presentation.matched_surfaces,
                               'strategy_version', presentation.strategy_version
                           ),
                           'forms', editor.forms,
                           'meanings', editor.meanings
                       ) AS snapshot,
                       'draft'::text AS status,
                       1::smallint AS status_rank,
                       presentation.label AS sort_headword
                FROM lexicon.entries entry
                JOIN lexicon.entry_editor_projection editor
                  ON editor.entry_id = entry.id
                 AND editor.rebuilt_revision = entry.revision
                JOIN lexicon.entry_presentation_projection presentation
                  ON presentation.entry_id = entry.id
                 AND presentation.content_schema_version = 3
                 AND presentation.source_revision = entry.revision
                WHERE $12::boolean
                  AND entry.content_schema_version = 3
                  AND entry.current_publication_id IS NULL
                  AND entry.archived_at IS NULL
            )
            SELECT id AS entry_id,
                   kind,
                   content_schema_version,
                   snapshot,
                   status,
                   status_rank,
                   COALESCE((
                       SELECT array_agg(DISTINCT labels.code ORDER BY labels.code)
                       FROM (
                           SELECT part.code
                           FROM lexicon.entry_publication_part_of_speech_refs pos_ref
                           JOIN catalog.parts_of_speech part
                             ON part.id = pos_ref.part_of_speech_id
                           WHERE searchable_entry.status = 'published'
                             AND pos_ref.publication_id = searchable_entry.publication_id
                             AND pos_ref.entry_id = searchable_entry.id
                           UNION
                           SELECT part.code
                           FROM lexicon.entry_pos pos
                           JOIN catalog.parts_of_speech part
                             ON part.id = pos.part_of_speech_id
                           WHERE searchable_entry.status = 'draft'
                             AND pos.entry_id = searchable_entry.id
                       ) labels
                   ), ARRAY[]::text[]) AS pos_labels,
                   sort_headword,
                   count(*) OVER() AS total
            FROM searchable_entry
            WHERE ($11::boolean OR content_schema_version = 2)
              AND ($2::text IS NULL OR kind = $2)
              AND CASE WHEN $3 THEN
                    EXISTS (
                        SELECT 1
                        FROM lexicon.surface_sources surface
                        WHERE surface.entry_id = searchable_entry.id
                          AND surface.content_schema_version = searchable_entry.content_schema_version
                          AND (
                              (
                                  searchable_entry.status = 'published'
                                  AND surface.publication_id = searchable_entry.publication_id
                                  AND surface.content_scope = 'current_publication'
                              )
                              OR (
                                  searchable_entry.status = 'draft'
                                  AND surface.publication_id IS NULL
                                  AND surface.content_scope = 'draft'
                                  AND surface.source_revision = searchable_entry.source_revision
                              )
                          )
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
                          AND surface.content_schema_version = searchable_entry.content_schema_version
                          AND (
                              (
                                  searchable_entry.status = 'published'
                                  AND surface.publication_id = searchable_entry.publication_id
                                  AND surface.content_scope = 'current_publication'
                              )
                              OR (
                                  searchable_entry.status = 'draft'
                                  AND surface.publication_id IS NULL
                                  AND surface.content_scope = 'draft'
                                  AND surface.source_revision = searchable_entry.source_revision
                              )
                          )
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
                          AND surface.content_schema_version = searchable_entry.content_schema_version
                          AND (
                              (
                                  searchable_entry.status = 'published'
                                  AND surface.publication_id = searchable_entry.publication_id
                                  AND surface.content_scope = 'current_publication'
                              )
                              OR (
                                  searchable_entry.status = 'draft'
                                  AND surface.publication_id IS NULL
                                  AND surface.content_scope = 'draft'
                                  AND surface.source_revision = searchable_entry.source_revision
                              )
                          )
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
                    status_rank,
                    id
                  ) > ($6, $7 COLLATE "C", $8, $9)
              )
            ORDER BY kind ASC, sort_headword COLLATE "C" ASC, status_rank ASC, id ASC
            LIMIT $10
            "#,
        )
        .bind(pattern)
        .bind(filter.kind.map(kind_string))
        .bind(filter.exact)
        .bind(filter.exclude_exact)
        .bind(filter.q)
        .bind(filter.last_kind.map(kind_string))
        .bind(filter.last_headword)
        .bind(filter.last_status_rank)
        .bind(filter.last_word_id)
        .bind(filter.limit)
        .bind(filter.include_v3)
        .bind(filter.include_drafts)
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
                   entry.created_by_admin_id AS created_by,
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
              -- Native V3 empty shells remain addressable by ID, but they are not
              -- dictionary rows until the current draft projects a real surface.
              -- Published V3 entries stay visible even while a newer draft is incomplete.
              AND (
                    entry.content_schema_version = 2
                    OR entry.current_publication_id IS NOT NULL
                    OR EXISTS (
                        SELECT 1
                        FROM lexicon.surface_sources visible_surface
                        WHERE visible_surface.entry_id = entry.id
                          AND visible_surface.content_schema_version = 3
                          AND visible_surface.content_scope = 'draft'
                          AND visible_surface.source_revision = entry.revision
                          AND visible_surface.source_kind = 'form_variant'
                          AND visible_surface.is_deleted = FALSE
                    )
                  )
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
              AND (
                    entry.content_schema_version = 2
                    OR entry.current_publication_id IS NOT NULL
                    OR EXISTS (
                        SELECT 1
                        FROM lexicon.surface_sources visible_surface
                        WHERE visible_surface.entry_id = entry.id
                          AND visible_surface.content_schema_version = 3
                          AND visible_surface.content_scope = 'draft'
                          AND visible_surface.source_revision = entry.revision
                          AND visible_surface.source_kind = 'form_variant'
                          AND visible_surface.is_deleted = FALSE
                    )
                  )
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
            FROM lexicon.entries entry
            WHERE entry.archived_at IS NULL
              AND ($1 OR entry.content_schema_version = 2)
              AND (
                    entry.content_schema_version = 2
                    OR entry.current_publication_id IS NOT NULL
                    OR EXISTS (
                        SELECT 1
                        FROM lexicon.surface_sources visible_surface
                        WHERE visible_surface.entry_id = entry.id
                          AND visible_surface.content_schema_version = 3
                          AND visible_surface.content_scope = 'draft'
                          AND visible_surface.source_revision = entry.revision
                          AND visible_surface.source_kind = 'form_variant'
                          AND visible_surface.is_deleted = FALSE
                    )
                  )
            "#,
        )
        .bind(include_v3)
        .fetch_one(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// 批量查询一页词条各自「被谁引用」。**一次往返查完整页**——逐行标量子查询在
    /// 100 行 × 6 类下会退化成 600 次探查。
    ///
    /// 口径与 `delete_never_published_entry` 的入站引用拦截严格一致：
    /// 按引用方词条去重、含草稿引用、排除自引用。两处若各写一套迟早漂移，
    /// 就会出现「显示 0 引用却删不掉」——比不显示更糟。
    pub(crate) async fn entry_reference_rows(
        &self,
        entry_ids: &[Uuid],
    ) -> Result<Vec<EntryReferenceRow>, LexiconRepositoryError> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, EntryReferenceRow>(
            r#"
            WITH refs AS (
                -- 1 关联词已绑定：直接用 relations.target_entry_id，不要 JOIN nodes 取
                -- entry_id——JOIN 写法会退化成 Seq Scan，直接用则走 target_idx。
                SELECT relation.target_entry_id AS target_id,
                       relation.entry_id AS source_id,
                       'relation' AS kind
                  FROM lexicon.relations relation
                 WHERE relation.target_entry_id = ANY($1)
                   AND relation.entry_id <> relation.target_entry_id
                UNION ALL
                -- 2 关联词预绑定待物化
                SELECT relation.prebound_target_entry_id,
                       relation.entry_id,
                       'relation_prebound'
                  FROM lexicon.relations relation
                 WHERE relation.prebound_target_entry_id = ANY($1)
                   AND relation.entry_id <> relation.prebound_target_entry_id
                UNION ALL
                -- 3 例句关联已生效
                SELECT link.target_entry_id, link.entry_id, 'sentence_link'
                  FROM lexicon.sentence_links link
                 WHERE link.target_entry_id = ANY($1)
                   AND link.entry_id <> link.target_entry_id
                UNION ALL
                -- 4 已发布内容的词义引用
                SELECT sense_ref.target_entry_id, sense_ref.entry_id, 'publication_sense_ref'
                  FROM lexicon.entry_publication_sense_refs sense_ref
                 WHERE sense_ref.target_entry_id = ANY($1)
                   AND sense_ref.entry_id <> sense_ref.target_entry_id
                UNION ALL
                -- 5 例句关联待认领
                SELECT association.target_entry_id, association.entry_id, 'sentence_association'
                  FROM lexicon.sentence_associations association
                 WHERE association.target_entry_id = ANY($1)
                   AND association.entry_id <> association.target_entry_id
                UNION ALL
                -- 6 V3 短语把本词条当作成分
                SELECT usage.target_entry_id, usage.entry_id, 'phrase_component'
                  FROM lexicon.v3_phrase_variant_component_usages usage
                 WHERE usage.target_entry_id = ANY($1)
                   AND usage.entry_id <> usage.target_entry_id
            ),
            deduped AS (
                -- 同一引用方通过多条路径引用同一目标，只算一个依赖方；
                -- kind 取字典序最小者，保证结果稳定可测。
                SELECT target_id, source_id, min(kind) AS kind
                  FROM refs
                 WHERE target_id IS NOT NULL
                 GROUP BY target_id, source_id
            ),
            enriched AS (
                SELECT deduped.target_id,
                       deduped.source_id,
                       deduped.kind,
                       COALESCE((
                           SELECT string_agg(headword.headword, ' / ' ORDER BY CASE
                               WHEN headword.dialect = 'common' THEN 0
                               WHEN headword.dialect = source.source_dialect THEN 1
                               WHEN headword.dialect = 'uk' THEN 2
                               ELSE 3 END)
                           FROM lexicon.entry_headwords headword
                           WHERE headword.entry_id = source.id
                       ), '') AS source_headword,
                       CASE
                           WHEN source.archived_at IS NOT NULL THEN 'archived'
                           WHEN source.current_publication_id IS NOT NULL THEN 'published'
                           ELSE 'draft'
                       END AS source_status
                  FROM deduped
                  JOIN lexicon.entries source ON source.id = deduped.source_id
            ),
            ranked AS (
                SELECT enriched.*,
                       count(*) OVER (PARTITION BY target_id) AS total,
                       row_number() OVER (
                           PARTITION BY target_id
                           ORDER BY source_headword, source_id
                       ) AS position
                  FROM enriched
            )
            SELECT target_id, source_id, kind, source_headword, source_status, total
              FROM ranked
             WHERE position <= 5
             ORDER BY target_id, position
            "#,
        )
        .bind(entry_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }
}
