-- Old binaries reject extended revision snapshots. Refuse a lossy rollback once used.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM lexicon_next.draft_revisions r,
        LATERAL jsonb_array_elements(r.document->'senses') s
        WHERE jsonb_array_length(COALESCE(s->'no_equivalents', '[]'::jsonb)) > 0
           OR EXISTS (SELECT 1 FROM jsonb_array_elements(s->'equivalents') e WHERE e ? 'metadata')
    ) THEN
        RAISE EXCEPTION 'equivalent metadata is present in retained revisions; compatible data rollback required';
    END IF;
END $$;
DROP TABLE lexicon_next.equivalent_taxonomy_refs;
ALTER TABLE lexicon_next.equivalents DROP COLUMN metadata;
