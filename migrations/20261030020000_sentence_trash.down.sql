-- Refuse to discard recoverable drafts during rollback. Restore or purge them first.
DO $$ BEGIN IF EXISTS (SELECT 1 FROM lexicon.shared_sentences WHERE in_trash) THEN RAISE EXCEPTION 'Restore or purge sentence trash before rollback'; END IF; END $$;
ALTER TABLE lexicon.shared_sentences DROP CONSTRAINT shared_sentence_trash_state;
ALTER TABLE lexicon.shared_sentences DROP COLUMN in_trash;
