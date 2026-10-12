-- Oversample at most 32 nearest surfaces before deduplication/eligibility checks.
-- A bounded correction fallback is preferable to scanning all similar phrases
-- to prove there are no more candidates. Exact/prefix matching is unaffected.
WITH nearest AS MATERIALIZED (
    SELECT s.entry_id,s.publication_id,s.surface,s.surface <-> $1 AS distance
    FROM lexicon.surface_sources s
    WHERE s.language='en' AND s.normalization_version=1 AND char_length(s.surface)>=6
      AND s.content_schema_version=3 AND s.source_kind='form_variant'
      AND s.content_scope='current_publication' AND NOT s.is_deleted
      AND NOT (s.entry_id=ANY($2::uuid[])) AND s.surface % $1
    ORDER BY s.surface <-> $1
    LIMIT 32
)
SELECT s.entry_id
FROM nearest s
JOIN lexicon.entries e ON e.id=s.entry_id AND e.current_publication_id=s.publication_id
WHERE e.archived_at IS NULL AND e.content_schema_version=3
  AND abs(char_length(s.surface)-char_length($1)) <= greatest(2,char_length($1)/3)
GROUP BY s.entry_id
ORDER BY min(s.distance),s.entry_id
LIMIT $3
