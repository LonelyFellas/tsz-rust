use super::*;

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
    pub(crate) async fn published_component_target_surfaces(
        tx: &mut Transaction<'_, Postgres>,
        dialect_scopes: &[String],
        keyword: &str,
        kind: Option<EntryKind>,
        limit: i64,
    ) -> Result<Vec<SentenceDiscoverySurfaceRecord>, LexiconRepositoryError> {
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
              AND source.surface ILIKE $3 ESCAPE '\'
              AND ($4::text IS NULL OR source.entry_kind = $4::text)
              AND source.pos_id IS NOT NULL
              AND source.pos IS NOT NULL
              AND COALESCE(source.form_id, source.source_node_id) IS NOT NULL
            ORDER BY source.normalized_surface, source.entry_id,
                     source.pos_id, matched_form_id, source.event_offset
            LIMIT $5
            "#,
        )
        .bind(HEADWORD_NORMALIZATION_VERSION)
        .bind(dialect_scopes)
        .bind(format!("%{}%", escape_like_literal(keyword)))
        .bind(kind.map(kind_string))
        .bind(limit)
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
