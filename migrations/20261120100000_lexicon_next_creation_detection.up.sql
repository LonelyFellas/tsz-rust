ALTER TABLE lexicon_next.entries
    ADD COLUMN creation_detection jsonb;

ALTER TABLE lexicon_next.entries
    ADD CONSTRAINT lexicon_next_entries_creation_detection_object
    CHECK (
        creation_detection IS NULL
        OR jsonb_typeof(creation_detection) = 'object'
    );

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon_next.draft_revisions
        WHERE document ? 'creation_detection'
          AND jsonb_typeof(document->'creation_detection') <> 'object'
    ) THEN
        RAISE EXCEPTION 'invalid lexicon-next creation detection snapshot';
    END IF;
END
$$;

UPDATE lexicon_next.entries e
SET creation_detection = r.document->'creation_detection'
FROM lexicon_next.draft_revisions r
WHERE r.entry_id = e.id
  AND r.revision = e.revision
  AND r.document ? 'creation_detection';

UPDATE lexicon_next.entries e
SET creation_detection = p.snapshot->'draft'->'creation_detection'
FROM lexicon_next.publications p
WHERE p.entry_id = e.id
  AND p.snapshot->'draft' ? 'creation_detection'
  AND p.lifecycle_revision = (
      SELECT max(latest.lifecycle_revision)
      FROM lexicon_next.publications latest
      WHERE latest.entry_id = e.id
  )
  AND e.creation_detection IS NULL;

UPDATE lexicon_next.draft_revisions
SET document = document - 'creation_detection'
WHERE document ? 'creation_detection';
