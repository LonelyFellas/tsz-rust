use super::*;

const SURFACE_SOURCES_QUERY: &str = r#"
    WITH requested AS (
        SELECT value.dialect_scope,
               value.normalized_surface,
               value.ordinality
        FROM unnest($2::text[], $3::text[])
             WITH ORDINALITY AS value(
                 dialect_scope,
                 normalized_surface,
                 ordinality
             )
    )
    SELECT requested.dialect_scope AS matched_dialect_scope,
           source.entry_id,
           COALESCE(entry_headword.headword, presentation.label) AS entry_headword,
           COALESCE(entry_headword.dialect, source.dialect) AS entry_headword_dialect,
           source.entry_kind,
           CASE
               WHEN entry.archived_at IS NOT NULL THEN 'archived'
               WHEN entry.current_publication_id IS NOT NULL THEN 'published'
               ELSE 'draft'
           END AS lifecycle_status,
           source.source_id,
           source.source_kind,
           source.source_node_id,
           source.content_scope,
           source.publication_id,
           source.surface,
           source.normalized_surface,
           source.dialect,
           source.normalization_version,
           source.source_revision,
           source.event_offset,
           source.pos_id,
           source.pos,
           source.form_type
    FROM requested
    JOIN lexicon.surface_sources source
      ON source.language = $1
     AND source.dialect_scope = requested.dialect_scope
     AND source.normalized_surface = requested.normalized_surface
     AND source.is_deleted = FALSE
    JOIN lexicon.entries entry ON entry.id = source.entry_id
    LEFT JOIN LATERAL (
        SELECT headword.surface AS headword, headword.dialect
        FROM lexicon.surface_sources headword
        WHERE headword.entry_id = source.entry_id
          AND headword.source_kind = 'headword'
          AND headword.content_scope = source.content_scope
          AND headword.publication_id IS NOT DISTINCT FROM source.publication_id
          AND headword.dialect_scope = source.dialect_scope
          AND headword.is_deleted = FALSE
        ORDER BY CASE
                     WHEN headword.dialect = 'common' THEN 0
                     WHEN headword.dialect = source.dialect_scope THEN 1
                     ELSE 2
                 END,
                 headword.dialect,
                 headword.source_id
        LIMIT 1
    ) entry_headword ON TRUE
    LEFT JOIN lexicon.entry_presentation_projection presentation
      ON presentation.entry_id = source.entry_id
     AND presentation.content_schema_version = 3
    WHERE ($4::uuid IS NULL OR source.entry_id <> $4)
      AND COALESCE(entry_headword.headword, presentation.label) IS NOT NULL
      AND (
          source.content_scope = 'draft'
          OR (
              source.content_scope = 'current_publication'
              AND source.publication_id = entry.current_publication_id
          )
      )
    ORDER BY requested.ordinality,
             source.entry_kind,
             source.entry_id,
             source.source_kind,
             source.source_id,
             source.content_scope,
             source.publication_id
"#;

const SURFACE_ENTRY_CONTEXTS_QUERY: &str = r#"
    SELECT entry.id AS entry_id,
           entry.content_schema_version,
           projection.forms,
           projection.meanings,
           entry.updated_at
    FROM lexicon.entries entry
    JOIN lexicon.entry_editor_projection projection
      ON projection.entry_id = entry.id
    WHERE entry.id = ANY($1)
    ORDER BY entry.id
"#;

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
               source_entry.headword_mode AS source_headword_mode,
               source_entry.source_dialect,
               (SELECT headword FROM lexicon.entry_headwords
                WHERE entry_id = source_entry.id AND dialect = 'common') AS source_common_headword,
               (SELECT headword FROM lexicon.entry_headwords
                WHERE entry_id = source_entry.id AND dialect = 'uk') AS source_uk_headword,
               (SELECT headword FROM lexicon.entry_headwords
                WHERE entry_id = source_entry.id AND dialect = 'us') AS source_us_headword,
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
          -- 草稿态关系（未发布词条、或已发布词条工作区里未发布的修订）只对源词条
          -- 创建者可见；外人经下方发布分支看该源词条的已发布状态。
          AND source_entry.created_by_admin_id = $2

        UNION ALL

        SELECT reference.target_entry_id,
               reference.entry_id AS source_entry_id,
               reference.source_node_id,
               CASE
                   WHEN source_entry.archived_at IS NOT NULL THEN 'archived'
                   WHEN source_entry.current_publication_id IS NOT NULL THEN 'published'
                   ELSE 'draft'
               END AS source_status,
               source_entry.headword_mode AS source_headword_mode,
               source_entry.source_dialect,
               (SELECT headword FROM lexicon.entry_headwords
                WHERE entry_id = source_entry.id AND dialect = 'common') AS source_common_headword,
               (SELECT headword FROM lexicon.entry_headwords
                WHERE entry_id = source_entry.id AND dialect = 'uk') AS source_uk_headword,
               (SELECT headword FROM lexicon.entry_headwords
                WHERE entry_id = source_entry.id AND dialect = 'us') AS source_us_headword,
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
          -- 草稿行让位（去重）只在草稿行真的会展示给该 actor 时成立，否则外人会
          -- 「草稿行被滤掉、发布行又被吞」两头落空。
          AND (
              source_entry.created_by_admin_id <> $2
              OR NOT EXISTS (
                  SELECT 1
                  FROM lexicon.relations draft_relation
                  WHERE draft_relation.id = reference.source_node_id
                    AND draft_relation.entry_id = reference.entry_id
                    AND draft_relation.target_entry_id = reference.target_entry_id
                    AND draft_relation.target_sense_id = reference.target_sense_id
              )
          )
    )
    SELECT target_entry_id,
           source_entry_id,
           source_node_id,
           source_status,
           source_headword_mode,
           source_dialect,
           source_common_headword,
           source_uk_headword,
           source_us_headword,
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

    /// Return the authoritative, kind-independent surface sources for the
    /// requested normalized dialect scopes.
    ///
    /// The projection retains tombstones and superseded publication rows for
    /// migration/catch-up safety. This reader deliberately filters both and
    /// only exposes draft plus the entry's current publication. Archived
    /// entries remain visible and are identified by `lifecycle_status`.
    ///
    /// 注意：draft-scope 行在这里**不**按创建者过滤——这是「V2 入口不做草稿可见性
    /// 收口」的既定拍板（生产无 V2 数据，随 legacy 退役消亡）。V3 入口一律走
    /// `v3_surface_material_in`（按 `visible_to` 过滤他人未发布草稿），新调用方不要
    /// 因为本方法自称 authoritative 就拿它喂 V3 流程。
    pub async fn surface_sources(
        &self,
        language: &str,
        requested: &[SurfaceLookupKey],
        excluding_entry_id: Option<Uuid>,
    ) -> Result<Vec<SurfaceSourceRecord>, LexiconRepositoryError> {
        if requested.is_empty() {
            return Ok(Vec::new());
        }

        let dialect_scopes = requested
            .iter()
            .map(|key| key.dialect_scope.as_str())
            .collect::<Vec<_>>();
        let normalized_surfaces = requested
            .iter()
            .map(|key| key.normalized_surface.as_str())
            .collect::<Vec<_>>();

        sqlx::query_as::<_, SurfaceSourceRecord>(SURFACE_SOURCES_QUERY)
            .bind(language)
            .bind(dialect_scopes)
            .bind(normalized_surfaces)
            .bind(excluding_entry_id)
            .fetch_all(&self.pool)
            .await
            .map_err(LexiconRepositoryError::Database)
    }

    /// Queries current surface sources on the caller's transaction. Lock-held
    /// create revalidation must use this path instead of borrowing from the pool.
    pub async fn surface_sources_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        language: &str,
        requested: &[SurfaceLookupKey],
        excluding_entry_id: Option<Uuid>,
    ) -> Result<Vec<SurfaceSourceRecord>, LexiconRepositoryError> {
        if requested.is_empty() {
            return Ok(Vec::new());
        }
        let dialect_scopes = requested
            .iter()
            .map(|key| key.dialect_scope.as_str())
            .collect::<Vec<_>>();
        let normalized_surfaces = requested
            .iter()
            .map(|key| key.normalized_surface.as_str())
            .collect::<Vec<_>>();
        sqlx::query_as::<_, SurfaceSourceRecord>(SURFACE_SOURCES_QUERY)
            .bind(language)
            .bind(dialect_scopes)
            .bind(normalized_surfaces)
            .bind(excluding_entry_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn surface_entry_contexts(
        &self,
        entry_ids: &[Uuid],
    ) -> Result<Vec<SurfaceEntryContextRecord>, LexiconRepositoryError> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, SurfaceEntryContextRecord>(SURFACE_ENTRY_CONTEXTS_QUERY)
            .bind(entry_ids)
            .fetch_all(&self.pool)
            .await
            .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn surface_entry_contexts_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        entry_ids: &[Uuid],
    ) -> Result<Vec<SurfaceEntryContextRecord>, LexiconRepositoryError> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, SurfaceEntryContextRecord>(SURFACE_ENTRY_CONTEXTS_QUERY)
            .bind(entry_ids)
            .fetch_all(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn surface_inbound_relations(
        &self,
        target_entry_ids: &[Uuid],
        visible_to: Uuid,
    ) -> Result<Vec<SurfaceInboundRelationRecord>, LexiconRepositoryError> {
        if target_entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, SurfaceInboundRelationRecord>(SURFACE_INBOUND_RELATIONS_QUERY)
            .bind(target_entry_ids)
            .bind(visible_to)
            .fetch_all(&self.pool)
            .await
            .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn surface_inbound_relations_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        target_entry_ids: &[Uuid],
        visible_to: Uuid,
    ) -> Result<Vec<SurfaceInboundRelationRecord>, LexiconRepositoryError> {
        if target_entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, SurfaceInboundRelationRecord>(SURFACE_INBOUND_RELATIONS_QUERY)
            .bind(target_entry_ids)
            .bind(visible_to)
            .fetch_all(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)
    }
}
