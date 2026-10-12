-- Existing lookup btrees serve equality, and the surface trigram GIN serves
-- typo matching. Ordered normalized prefixes need a C-collation index to stop
-- after a few entries, even for broad prefixes or a generic prepared plan.
CREATE INDEX lexicon_surface_sources_suggestion_prefix_idx
ON lexicon.surface_sources (normalized_surface COLLATE "C",entry_id)
INCLUDE (publication_id)
WHERE content_scope='current_publication' AND NOT is_deleted
  AND content_schema_version=3 AND source_kind='form_variant'
  AND language='en' AND normalization_version=1;
