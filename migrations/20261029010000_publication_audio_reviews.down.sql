-- Review receipts are audit evidence. A populated deployment requires an explicit
-- retention/migration plan rather than silently losing confirmations on rollback.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM speech.publication_audio_reviews) THEN
        RAISE EXCEPTION 'publication audio review receipts must be retained';
    END IF;
END $$;
ALTER TABLE lexicon.entry_publications DROP COLUMN audio_review_id;
ALTER TABLE lexicon.shared_sentence_publications DROP COLUMN audio_review_id;
DROP TABLE speech.publication_audio_reviews;
