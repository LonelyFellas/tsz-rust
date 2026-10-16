SELECT q.row_number::bigint, matched.entry_id, matched.publication_id
FROM unnest($1::text[]) WITH ORDINALITY AS q(surface, row_number)
CROSS JOIN LATERAL (
    SELECT s.entry_id, s.publication_id
    FROM lexicon.surface_sources s
    JOIN lexicon.entries e ON e.id=s.entry_id AND e.current_publication_id=s.publication_id
    JOIN lexicon.entry_publications p ON p.id=s.publication_id AND p.entry_id=s.entry_id
    WHERE s.language='en' AND s.dialect_scope IN ('uk','us')
      AND s.normalization_version=1 AND s.content_schema_version=3 AND s.source_kind='form_variant'
      AND s.content_scope='current_publication' AND NOT s.is_deleted
      AND e.archived_at IS NULL AND p.content_schema_version=3
      AND s.normalized_surface=q.surface
    GROUP BY s.entry_id, s.publication_id
    ORDER BY s.entry_id
    LIMIT 3
) matched
ORDER BY q.row_number, matched.entry_id
