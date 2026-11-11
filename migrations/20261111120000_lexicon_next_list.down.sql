-- Refuse to silently reactivate archived records or discard annotations on rollback.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon_next.entries WHERE status='archived' OR annotation IS NOT NULL)
       OR EXISTS (SELECT 1 FROM lexicon_next.list_commands) THEN
        RAISE EXCEPTION 'lexicon-next list state exists; retain this migration when rolling back services';
    END IF;
END $$;
DROP TABLE lexicon_next.list_commands;
ALTER TABLE lexicon_next.entries DROP COLUMN created_at, DROP COLUMN status,
    DROP COLUMN lifecycle_revision, DROP COLUMN annotation, DROP COLUMN annotation_revision;
