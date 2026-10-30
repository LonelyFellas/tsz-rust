ALTER TABLE lexicon.shared_sentences ADD COLUMN in_trash boolean NOT NULL DEFAULT false;
ALTER TABLE lexicon.shared_sentences ADD CONSTRAINT shared_sentence_trash_state CHECK (NOT in_trash OR (deleted_at IS NOT NULL AND current_publication_id IS NULL));
