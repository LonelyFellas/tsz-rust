-- Origin is immutable provenance, not a live reference. Deleting an unreferenced
-- source draft must not update the independently published sentence row.
ALTER TABLE lexicon.shared_sentences DROP CONSTRAINT shared_sentences_source_entry_id_fkey;
