-- Reverting requires all origin entries still to exist; do not erase sentences to force rollback.
ALTER TABLE lexicon.shared_sentences ADD CONSTRAINT shared_sentences_source_entry_id_fkey
    FOREIGN KEY (source_entry_id) REFERENCES lexicon.entries(id) ON DELETE SET NULL;
