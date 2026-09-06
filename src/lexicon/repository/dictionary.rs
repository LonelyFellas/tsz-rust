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

    pub(crate) async fn dictionary_term_from_region_surface(
        &self,
        normalized: &str,
    ) -> Result<Option<DictionaryTermRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, DictionaryTermRecord>(
            r#"
            SELECT surfaces.term,
                   CASE WHEN strpos(surfaces.term, ' ') > 0 THEN 'phrase' ELSE 'word' END AS kind,
                   surfaces.pos, surfaces.region_family,
                   datasets.source_name AS provider_name,
                   datasets.source_version AS provider_version
            FROM dictionary.active_region_surfaces surfaces
            JOIN dictionary.datasets datasets ON datasets.id = surfaces.dataset_id
            WHERE surfaces.normalized_term = $1
            "#,
        )
        .bind(normalized)
        .fetch_optional(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn dictionary_contents_for_terms(
        &self,
        normalized: &[String],
    ) -> Result<Vec<DictionaryContentRecord>, LexiconRepositoryError> {
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, DictionaryContentRecord>(
            r#"
            SELECT content.normalized_term, content.pos, content.senses,
                   content.forms, content.sounds,
                   datasets.source_name AS provider_name,
                   content_import.source_version AS provider_version
            FROM dictionary.entry_contents content
            JOIN dictionary.datasets datasets ON datasets.id = content.dataset_id
            JOIN dictionary.content_imports content_import
              ON content_import.dataset_id = content.dataset_id
            WHERE datasets.status = 'active' AND content.normalized_term = ANY($1)
            ORDER BY content.normalized_term, content.source_key
            "#,
        )
        .bind(normalized)
        .fetch_all(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn region_surface(
        &self,
        normalized: &str,
    ) -> Result<Option<RegionSurfaceRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, RegionSurfaceRecord>(
            r#"
            SELECT normalized_term, term, region_family, pos, targets, is_headword
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
            SELECT normalized_term, term, region_family, pos, targets, is_headword
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

    pub(crate) async fn region_surfaces_targeting(
        &self,
        normalized: &str,
    ) -> Result<Vec<RegionSurfaceRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, RegionSurfaceRecord>(
            r#"
            SELECT normalized_term, term, region_family, pos, targets, is_headword
            FROM dictionary.active_region_surfaces
            WHERE targets @> ARRAY[$1]::TEXT[]
            ORDER BY normalized_term
            LIMIT 64
            "#,
        )
        .bind(normalized)
        .fetch_all(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn region_evidence(
        &self,
        normalized: &[String],
    ) -> Result<Vec<RegionEvidenceRecord>, LexiconRepositoryError> {
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, RegionEvidenceRecord>(
            r#"
            SELECT evidence.normalized_term, evidence.evidence_type,
                   evidence.raw_tags, evidence.pos, evidence.targets
            FROM dictionary.region_evidence evidence
            JOIN dictionary.datasets datasets ON datasets.id = evidence.dataset_id
            WHERE datasets.status = 'active'
              AND evidence.normalized_term = ANY($1)
            ORDER BY evidence.normalized_term, evidence.id
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

    pub(crate) async fn dictionary_candidates_v3(
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
            UNION ALL
            SELECT surfaces.normalized_term, surfaces.term, surfaces.region_family
            FROM dictionary.active_region_surfaces surfaces
            WHERE surfaces.normalized_term = ANY($1)
              AND NOT EXISTS (
                  SELECT 1
                  FROM dictionary.active_terms terms
                  WHERE terms.normalized_term = surfaces.normalized_term
              )
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
