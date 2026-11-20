DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon_next.entries
        WHERE creation_detection IS NOT NULL
    ) THEN
        RAISE EXCEPTION 'lexicon-next creation detection snapshots exist; rollback would lose data';
    END IF;
END
$$;

ALTER TABLE lexicon_next.entries
    DROP CONSTRAINT lexicon_next_entries_creation_detection_object,
    DROP COLUMN creation_detection;
