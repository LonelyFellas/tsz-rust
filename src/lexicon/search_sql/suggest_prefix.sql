-- No GROUP BY/top-N sort over the full match set. The caller deduplicates the
-- small result and repeats with selected entry IDs excluded (at most 3 rounds).
SELECT s.entry_id
FROM lexicon.surface_sources s
JOIN LATERAL (
    SELECT e.id FROM lexicon.entries e
    WHERE e.id=s.entry_id AND e.current_publication_id=s.publication_id
      AND e.archived_at IS NULL AND e.content_schema_version=3
    LIMIT 1
) visible ON true
WHERE s.language='en'
  AND s.normalization_version=1 AND s.content_schema_version=3 AND s.source_kind='form_variant'
  AND s.content_scope='current_publication' AND NOT s.is_deleted
  AND NOT (s.entry_id=ANY($2::uuid[]))
  AND s.normalized_surface COLLATE "C">=$1
  AND s.normalized_surface COLLATE "C"<$4
ORDER BY s.normalized_surface COLLATE "C",s.entry_id
LIMIT $3
