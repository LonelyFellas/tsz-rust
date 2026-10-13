-- Refuse to discard authored content or its immutable review history.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM wordlist_items WHERE author_note <> '')
       OR EXISTS (SELECT 1 FROM wordlist_review_requests WHERE author_notes <> '{}'::jsonb) THEN
        RAISE EXCEPTION 'wordlist author annotations must be preserved before rollback';
    END IF;
END $$;
ALTER TABLE wordlist_review_requests DROP COLUMN author_notes;
ALTER TABLE wordlist_items DROP COLUMN author_note;
