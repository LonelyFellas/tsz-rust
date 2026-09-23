use super::*;
use crate::lexicon::model::ComponentTargetEntryMatchRecord;

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
    /// 按关键字查询当前发布或草稿的完整成分目标。
    pub(crate) async fn component_target_entry_matches(
        tx: &mut Transaction<'_, Postgres>,
        dialect_scopes: &[String],
        keyword: &str,
        kind: Option<EntryKind>,
        entry_id: Option<Uuid>,
        exact: bool,
        drafts: bool,
    ) -> Result<Vec<ComponentTargetEntryMatchRecord>, LexiconRepositoryError> {
        let lowered = keyword.to_lowercase();
        let scope_join = if drafts {
            r#"JOIN lexicon.entries entry
                  ON entry.id = source.entry_id
                 AND entry.archived_at IS NULL
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
            SELECT source.entry_id,
                   MIN(CASE
                         WHEN source.normalized_surface = $5 THEN 0
                         WHEN source.normalized_surface LIKE $6 ESCAPE '\' THEN 1
                         ELSE 2
                       END)::int4 AS match_rank
            FROM lexicon.surface_sources source
            {scope_join}
              AND source.language = 'en'
              AND source.normalization_version = $1
              AND source.dialect_scope = ANY($2::text[])
              AND {match_predicate}
              AND ($4::text IS NULL OR source.entry_kind = $4::text)
              AND ($7::uuid IS NULL OR source.entry_id = $7)
              AND source.pos_id IS NOT NULL
              AND source.pos IS NOT NULL
              AND COALESCE(source.form_id, source.source_node_id) IS NOT NULL
            GROUP BY source.entry_id
            ORDER BY match_rank, source.entry_id
            "#
        );
        sqlx::query_as::<_, ComponentTargetEntryMatchRecord>(sqlx::AssertSqlSafe(sql))
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
            .bind(entry_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)
    }

    /// 返回一个有界词条批次的去重词面标识。排序按等于、前缀、其余词面，与 service 的
    /// 匹配等级一致；event_offset 只取最小值，因为它不参与关键字候选身份。
    /// 关键字检索的词面行。`exact` 为真时 `keyword` 须已是归一化 key，按 `normalized_surface` 等值；
    /// 否则按 `surface ILIKE '%keyword%'` 包含匹配。`drafts` 为真时查当前 V3 草稿词面
    /// （`content_scope = 'draft'`，不按创建者过滤，`publication_id` 为 NULL），否则查当前发布词面。
    pub(crate) async fn component_target_surfaces(
        tx: &mut Transaction<'_, Postgres>,
        dialect_scopes: &[String],
        keyword: &str,
        kind: Option<EntryKind>,
        entry_ids: &[Uuid],
        exact: bool,
        drafts: bool,
    ) -> Result<Vec<SentenceDiscoverySurfaceRecord>, LexiconRepositoryError> {
        let lowered = keyword.to_lowercase();
        let scope_join = if drafts {
            r#"JOIN lexicon.entries entry
                  ON entry.id = source.entry_id
                 AND entry.archived_at IS NULL
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
                SELECT
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
                       MIN(source.event_offset) AS event_offset
                FROM lexicon.surface_sources source
                {scope_join}
                  AND source.language = 'en'
                  AND source.normalization_version = $1
                  AND source.dialect_scope = ANY($2::text[])
                  AND {match_predicate}
                  AND ($4::text IS NULL OR source.entry_kind = $4::text)
                  AND source.entry_id = ANY($7::uuid[])
                  AND source.pos_id IS NOT NULL
                  AND source.pos IS NOT NULL
                  AND COALESCE(source.form_id, source.source_node_id) IS NOT NULL
                GROUP BY source.normalized_surface, source.surface, source.entry_kind,
                         source.entry_id, source.publication_id, source.pos_id, source.pos,
                         COALESCE(source.form_id, source.source_node_id), source.source_node_id,
                         source.dialect_scope
            ) matched
            ORDER BY CASE
                         WHEN matched.normalized_surface = $5 THEN 0
                         WHEN matched.normalized_surface LIKE $6 ESCAPE '\' THEN 1
                         ELSE 2
                     END,
                     matched.normalized_surface, matched.entry_id,
                     matched.pos_id, matched.matched_form_id, matched.event_offset
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
            .bind(entry_ids)
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

    /// 关键字检索用：批量取当前 V3 草稿目标，不加锁（只读事务）、不按创建者过滤。
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
            ORDER BY entry.id"
        )))
        .bind(entry_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }
}
