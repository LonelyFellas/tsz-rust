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
           entry_headword.headword AS entry_headword,
           entry_headword.dialect AS entry_headword_dialect,
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
    JOIN LATERAL (
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
    WHERE ($4::uuid IS NULL OR source.entry_id <> $4)
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
    SELECT reference.target_entry_id,
           reference.entry_id AS source_entry_id,
           reference.source_node_id,
           publication.snapshot AS source_snapshot
    FROM lexicon.entry_publication_sense_refs reference
    JOIN lexicon.entries source_entry
      ON source_entry.id = reference.entry_id
     AND source_entry.current_publication_id = reference.publication_id
     AND source_entry.archived_at IS NULL
    JOIN lexicon.entry_publications publication
      ON publication.id = reference.publication_id
     AND publication.entry_id = reference.entry_id
    WHERE reference.target_entry_id = ANY($1)
      AND reference.reference_kind = 'relation'
    ORDER BY reference.target_entry_id,
             reference.entry_id,
             reference.source_node_id
"#;

impl LexiconRepository {
    /// Return the authoritative, kind-independent surface sources for the
    /// requested normalized dialect scopes.
    ///
    /// The projection retains tombstones and superseded publication rows for
    /// migration/catch-up safety. This reader deliberately filters both and
    /// only exposes draft plus the entry's current publication. Archived
    /// entries remain visible and are identified by `lifecycle_status`.
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
    ) -> Result<Vec<SurfaceInboundRelationRecord>, LexiconRepositoryError> {
        if target_entry_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, SurfaceInboundRelationRecord>(SURFACE_INBOUND_RELATIONS_QUERY)
            .bind(target_entry_ids)
            .fetch_all(&self.pool)
            .await
            .map_err(LexiconRepositoryError::Database)
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
