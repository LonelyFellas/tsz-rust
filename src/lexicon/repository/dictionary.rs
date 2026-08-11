use super::*;

impl LexiconRepository {
    pub(crate) async fn dictionary_term(
        &self,
        normalized: &str,
    ) -> Result<Option<DictionaryTermRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, DictionaryTermRecord>(
            r#"
            SELECT term, kind, pos, region_family
            FROM dictionary.active_terms
            WHERE normalized_term = $1
            "#,
        )
        .bind(normalized)
        .fetch_optional(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn region_surface(
        &self,
        normalized: &str,
    ) -> Result<Option<RegionSurfaceRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, RegionSurfaceRecord>(
            r#"
            SELECT normalized_term, term, region_family, targets
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
            SELECT normalized_term, term, region_family, targets
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

    pub(crate) async fn duplicates(
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
                   headword.dialect
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
                ORDER BY CASE value.dialect WHEN 'common' THEN 0 WHEN keys.dialect_scope THEN 1 ELSE 2 END
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
}
