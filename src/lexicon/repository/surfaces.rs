use super::*;

const SURFACE_INBOUND_RELATIONS_QUERY: &str = r#"
    WITH inbound AS (
        SELECT relation.target_entry_id,
               relation.entry_id AS source_entry_id,
               relation.id AS source_node_id,
               CASE
                   WHEN source_entry.archived_at IS NOT NULL THEN 'archived'
                   WHEN source_entry.current_publication_id IS NOT NULL THEN 'published'
                   ELSE 'draft'
               END AS source_status,
               source_presentation.label AS source_presentation_label,
               relation.relation_type AS draft_relation_type,
               NULL::jsonb AS source_snapshot,
               0 AS scope_order
        FROM lexicon.relations relation
        JOIN lexicon.entries source_entry ON source_entry.id = relation.entry_id
        LEFT JOIN lexicon.entry_presentation_projection source_presentation
          ON source_presentation.entry_id = source_entry.id
         AND source_presentation.content_schema_version = 3
        WHERE relation.target_entry_id = ANY($1)
          -- 草稿态关系对所有管理员可见（2026-09-08 口径）：撞名检测要能说清
          -- 「这个词面已被谁的哪条草稿占着」。写权限另有守卫。

        UNION ALL

        SELECT reference.target_entry_id,
               reference.entry_id AS source_entry_id,
               reference.source_node_id,
               CASE
                   WHEN source_entry.archived_at IS NOT NULL THEN 'archived'
                   WHEN source_entry.current_publication_id IS NOT NULL THEN 'published'
                   ELSE 'draft'
               END AS source_status,
               source_presentation.label AS source_presentation_label,
               NULL::text AS draft_relation_type,
               publication.snapshot AS source_snapshot,
               1 AS scope_order
        FROM lexicon.entry_publication_sense_refs reference
        JOIN lexicon.entries source_entry
          ON source_entry.id = reference.entry_id
         AND source_entry.current_publication_id = reference.publication_id
        LEFT JOIN lexicon.entry_presentation_projection source_presentation
          ON source_presentation.entry_id = source_entry.id
         AND source_presentation.content_schema_version = 3
        JOIN lexicon.entry_publications publication
          ON publication.id = reference.publication_id
         AND publication.entry_id = reference.entry_id
        WHERE reference.target_entry_id = ANY($1)
          AND reference.reference_kind = 'relation'
          -- 草稿行让位（去重）：草稿分支不再按 actor 过滤，所以有对应草稿行时
          -- 发布行一律让位，不必再问「这条草稿会不会展示给他」。
          AND NOT EXISTS (
              SELECT 1
              FROM lexicon.relations draft_relation
              WHERE draft_relation.id = reference.source_node_id
                AND draft_relation.entry_id = reference.entry_id
                AND draft_relation.target_entry_id = reference.target_entry_id
                AND draft_relation.target_sense_id = reference.target_sense_id
          )
    )
    SELECT target_entry_id,
           source_entry_id,
           source_node_id,
           source_status,
           source_presentation_label,
           draft_relation_type,
           source_snapshot
    FROM inbound
    ORDER BY target_entry_id, source_entry_id, source_node_id, scope_order
"#;

impl LexiconRepository {
    pub(crate) async fn active_headword_memberships_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        requested: &[crate::lexicon::visibility::VisibilityScope],
    ) -> Result<Vec<(crate::lexicon::visibility::VisibilityScope, Uuid)>, LexiconRepositoryError>
    {
        if requested.is_empty() {
            return Ok(Vec::new());
        }
        let languages = requested
            .iter()
            .map(|key| key.language.as_str())
            .collect::<Vec<_>>();
        let entry_kinds = requested
            .iter()
            .map(|key| key.entry_kind.as_str())
            .collect::<Vec<_>>();
        let dialect_scopes = requested
            .iter()
            .map(|key| key.dialect_scope.as_str())
            .collect::<Vec<_>>();
        let normalized = requested
            .iter()
            .map(|key| key.normalized_headword.as_str())
            .collect::<Vec<_>>();
        let rows = sqlx::query_as::<_, (String, String, String, String, Uuid)>(
            r#"
            WITH requested AS (
                SELECT * FROM unnest($1::text[], $2::text[], $3::text[], $4::text[])
                    AS value(language, entry_kind, dialect_scope, normalized_headword)
            )
            SELECT DISTINCT source.language,
                            source.entry_kind,
                            source.dialect_scope,
                            source.normalized_surface,
                            source.entry_id
            FROM requested
            JOIN lexicon.surface_sources source
              ON source.language = requested.language
             AND source.entry_kind = requested.entry_kind
             AND source.dialect_scope = requested.dialect_scope
             AND source.normalized_surface = requested.normalized_headword
             AND source.source_kind = 'headword'
             AND source.content_scope = 'current_publication'
             AND source.is_deleted = FALSE
            JOIN lexicon.entries entry
              ON entry.id = source.entry_id
             AND entry.archived_at IS NULL
             AND entry.current_publication_id = source.publication_id
            ORDER BY source.language, source.entry_kind, source.dialect_scope,
                     source.normalized_surface, source.entry_id
            "#,
        )
        .bind(languages)
        .bind(entry_kinds)
        .bind(dialect_scopes)
        .bind(normalized)
        .fetch_all(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        Ok(rows
            .into_iter()
            .map(
                |(language, entry_kind, dialect_scope, normalized_headword, id)| {
                    (
                        crate::lexicon::visibility::VisibilityScope {
                            language,
                            entry_kind,
                            dialect_scope,
                            normalized_headword,
                        },
                        id,
                    )
                },
            )
            .collect())
    }

    pub(crate) async fn surface_inbound_relations_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        target_entry_ids: &[Uuid],
    ) -> Result<Vec<SurfaceInboundRelationRecord>, LexiconRepositoryError> {
        if target_entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, SurfaceInboundRelationRecord>(SURFACE_INBOUND_RELATIONS_QUERY)
            .bind(target_entry_ids)
            .fetch_all(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)
    }
}
