-- GIN remains used by existing contains/similarity search. Autocomplete needs
-- nearest-neighbor ordering plus a small limit, which pg_trgm's GiST supports.
CREATE INDEX lexicon_surface_sources_suggestion_fuzzy_idx
ON lexicon.surface_sources USING gist (surface gist_trgm_ops(siglen=64))
WHERE content_scope='current_publication' AND NOT is_deleted
  AND content_schema_version=3 AND source_kind='form_variant'
  AND language='en' AND normalization_version=1 AND char_length(surface)>=6;

-- This partial predicate is explicit only in the long-phrase fallback. Short
-- or ordinary word queries retain the more selective existing GIN plan.
-- For an input of >=8 characters, the edit-length window never accepts <6.
