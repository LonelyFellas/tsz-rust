use super::*;

/// 成分用词 / 正文关联草稿目标的取数：当前草稿投影 + 展示词面 + （若有）当前发布快照。
/// SQL 只由常量片段拼成，绑定值全部走参数。
const COMPONENT_TARGET_DRAFT_SELECT: &str = r#"
        SELECT entry.id, entry.kind, entry.revision,
               COALESCE(presentation.label, '') AS label,
               projection.forms, projection.meanings,
               entry.current_publication_id,
               publication.snapshot AS current_snapshot,
               publication.source_revision AS current_revision
        FROM lexicon.entries entry
        JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
        LEFT JOIN lexicon.entry_presentation_projection presentation
          ON presentation.entry_id = entry.id
         AND presentation.content_schema_version = 3
        LEFT JOIN lexicon.entry_publications publication
          ON publication.id = entry.current_publication_id
         AND publication.entry_id = entry.id
         AND publication.content_schema_version = 3
"#;

impl LexiconRepository {
    pub(crate) async fn sentence_discovery_generation(
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<i64, LexiconRepositoryError> {
        sqlx::query_scalar(
            "SELECT generation FROM lexicon.sentence_discovery_generation WHERE singleton = TRUE",
        )
        .fetch_one(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn published_sentence_discovery_surfaces(
        tx: &mut Transaction<'_, Postgres>,
        dialect_scopes: &[String],
        normalized_surfaces: &[String],
    ) -> Result<Vec<SentenceDiscoverySurfaceRecord>, LexiconRepositoryError> {
        if normalized_surfaces.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, SentenceDiscoverySurfaceRecord>(
            r#"
            SELECT DISTINCT
                   source.normalized_surface,
                   source.surface,
                   source.entry_kind,
                   source.entry_id,
                   source.publication_id,
                   source.pos_id,
                   source.pos,
                   COALESCE(source.form_id, source.source_node_id) AS matched_form_id,
                   source.source_node_id AS matched_variant_id,
                   source.dialect_scope,
                   source.event_offset
            FROM lexicon.surface_sources source
            JOIN lexicon.entries entry
              ON entry.id = source.entry_id
             AND entry.archived_at IS NULL
             AND entry.current_publication_id = source.publication_id
            WHERE source.is_deleted = FALSE
              AND source.content_scope = 'current_publication'
              AND source.language = 'en'
              AND source.normalization_version = $1
              AND source.dialect_scope = ANY($2::text[])
              AND source.normalized_surface = ANY($3::text[])
              AND source.pos_id IS NOT NULL
              AND source.pos IS NOT NULL
              AND COALESCE(source.form_id, source.source_node_id) IS NOT NULL
            ORDER BY source.normalized_surface, source.entry_id,
                     source.pos_id, matched_form_id, source.event_offset
            "#,
        )
        .bind(HEADWORD_NORMALIZATION_VERSION)
        .bind(dialect_scopes)
        .bind(normalized_surfaces)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// 关键字检索短语成分目标：与 `published_sentence_discovery_surfaces` 共用同一套
    /// 「只看当前发布、未归档」的过滤，只把词面等值换成对 `surface` 的大小写不敏感包含匹配。
    /// 前置通配符用不上 `surface_sources` 上的 btree 索引，代价靠 `current_publication`
    /// 分片与 `limit` 框住。
    ///
    /// 排序下沉到 SQL：词面等于关键字的排最前、以关键字开头的其次、其余按词面。窗口只有
    /// `limit` 行，排序若留在 Rust 侧，点 `me` 时 `acme / came / home …` 会先把窗口占满，
    /// `me` 本身反而进不来。Rust 侧 `component_target_rank` 用同一规则给候选排序。
    /// 关键字检索的词面行。`exact` 为真时 `keyword` 须已是归一化 key，按 `normalized_surface` 等值；
    /// 否则按 `surface ILIKE '%keyword%'` 包含匹配。`drafts` 为真时查从未发布的 V3 草稿词面
    /// （`content_scope = 'draft'`，不按创建者过滤，`publication_id` 为 NULL），否则查当前发布词面。
    pub(crate) async fn component_target_surfaces(
        tx: &mut Transaction<'_, Postgres>,
        dialect_scopes: &[String],
        keyword: &str,
        kind: Option<EntryKind>,
        limit: i64,
        exact: bool,
        drafts: bool,
    ) -> Result<Vec<SentenceDiscoverySurfaceRecord>, LexiconRepositoryError> {
        let lowered = keyword.to_lowercase();
        let scope_join = if drafts {
            r#"JOIN lexicon.entries entry
                  ON entry.id = source.entry_id
                 AND entry.archived_at IS NULL
                 AND entry.current_publication_id IS NULL
                 AND entry.content_schema_version = 3
                WHERE source.is_deleted = FALSE
                  AND source.content_scope = 'draft'"#
        } else {
            r#"JOIN lexicon.entries entry
                  ON entry.id = source.entry_id
                 AND entry.archived_at IS NULL
                 AND entry.current_publication_id = source.publication_id
                WHERE source.is_deleted = FALSE
                  AND source.content_scope = 'current_publication'"#
        };
        let match_predicate = if exact {
            "source.normalized_surface = $3"
        } else {
            r"source.surface ILIKE $3 ESCAPE '\'"
        };
        let sql = format!(
            r#"
            SELECT matched.*
            FROM (
                SELECT DISTINCT
                       source.normalized_surface,
                       source.surface,
                       source.entry_kind,
                       source.entry_id,
                       source.publication_id,
                       source.pos_id,
                       source.pos,
                       COALESCE(source.form_id, source.source_node_id) AS matched_form_id,
                       source.source_node_id AS matched_variant_id,
                       source.dialect_scope,
                       source.event_offset
                FROM lexicon.surface_sources source
                {scope_join}
                  AND source.language = 'en'
                  AND source.normalization_version = $1
                  AND source.dialect_scope = ANY($2::text[])
                  AND {match_predicate}
                  AND ($4::text IS NULL OR source.entry_kind = $4::text)
                  AND source.pos_id IS NOT NULL
                  AND source.pos IS NOT NULL
                  AND COALESCE(source.form_id, source.source_node_id) IS NOT NULL
            ) matched
            ORDER BY CASE
                         WHEN matched.normalized_surface = $5 THEN 0
                         WHEN matched.normalized_surface LIKE $6 ESCAPE '\' THEN 1
                         ELSE 2
                     END,
                     matched.normalized_surface, matched.entry_id,
                     matched.pos_id, matched.matched_form_id, matched.event_offset
            LIMIT $7
            "#
        );
        // SQL 只由上面的常量片段拼成，绑定值全部走参数，没有用户输入进字符串。
        sqlx::query_as::<_, SentenceDiscoverySurfaceRecord>(sqlx::AssertSqlSafe(sql))
            .bind(HEADWORD_NORMALIZATION_VERSION)
            .bind(dialect_scopes)
            .bind(if exact {
                keyword.to_owned()
            } else {
                format!("%{}%", escape_like_literal(keyword))
            })
            .bind(kind.map(kind_string))
            .bind(&lowered)
            .bind(format!("{}%", escape_like_literal(&lowered)))
            .bind(limit)
            .fetch_all(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)
    }

    /// 成分用词 / 正文关联的草稿目标（单条，事务内加共享锁）：与发布时锁目标同款
    /// `FOR SHARE NOWAIT`，读到的草稿内容与随后记入引用的 entry revision 是同一版；
    /// 目标正在保存时立即 `TargetPublicationBusy`。归档 / 非 V3 / 非 word-phrase 视为不存在。
    pub(crate) async fn component_target_draft_for_share(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
    ) -> Result<Option<ComponentTargetDraftRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, ComponentTargetDraftRecord>(sqlx::AssertSqlSafe(format!(
            "{COMPONENT_TARGET_DRAFT_SELECT}
            WHERE entry.id = $1
              AND entry.content_schema_version = 3
              AND entry.kind IN ('word', 'phrase')
              AND entry.archived_at IS NULL
            FOR SHARE OF entry NOWAIT"
        )))
        .bind(entry_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_target_publication_lock_error)
    }

    /// 关键字检索用：批量取从未发布的 V3 草稿目标，不加锁（只读事务）、不按创建者过滤。
    pub(crate) async fn component_target_drafts(
        tx: &mut Transaction<'_, Postgres>,
        entry_ids: &[Uuid],
    ) -> Result<Vec<ComponentTargetDraftRecord>, LexiconRepositoryError> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, ComponentTargetDraftRecord>(sqlx::AssertSqlSafe(format!(
            "{COMPONENT_TARGET_DRAFT_SELECT}
            WHERE entry.id = ANY($1)
              AND entry.content_schema_version = 3
              AND entry.kind IN ('word', 'phrase')
              AND entry.archived_at IS NULL
              AND entry.current_publication_id IS NULL
            ORDER BY entry.id"
        )))
        .bind(entry_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn draft_sentence_discovery_targets(
        tx: &mut Transaction<'_, Postgres>,
        dialect_scopes: &[String],
        normalized_surface: &str,
        draft_created_by: Uuid,
    ) -> Result<Vec<SentenceDiscoveryDraftRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, SentenceDiscoveryDraftRecord>(
            r#"
            SELECT DISTINCT
                   entry.id AS entry_id,
                   entry.revision AS entry_revision,
                   COALESCE(presentation.label, source.surface) AS headword
            FROM lexicon.surface_sources source
            JOIN lexicon.entries entry
              ON entry.id = source.entry_id
             AND entry.archived_at IS NULL
             AND entry.content_schema_version = 3
             -- 未发布内容只对词条创建者可见：过滤作用于一切 draft-scope surface，
             -- 含已发布词条草稿里尚未发布的新词形（从严口径）。
             AND entry.created_by_admin_id = $4
            LEFT JOIN lexicon.entry_presentation_projection presentation
              ON presentation.entry_id = entry.id
            WHERE source.is_deleted = FALSE
              AND source.content_scope = 'draft'
              AND source.language = 'en'
              AND source.normalization_version = $1
              AND source.dialect_scope = ANY($2::text[])
              AND source.normalized_surface = $3
            ORDER BY entry.id
            "#,
        )
        .bind(HEADWORD_NORMALIZATION_VERSION)
        .bind(dialect_scopes)
        .bind(normalized_surface)
        .bind(draft_created_by)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }
}
