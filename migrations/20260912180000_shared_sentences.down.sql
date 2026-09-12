-- Refuse destructive rollback once independently published content exists.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.shared_sentences) THEN
        RAISE EXCEPTION 'shared sentences exist; export or explicitly remove their data before rollback';
    END IF;
END $$;
-- Rollback is only lossless before new shared sentences are authored. Export first in a live system.
DROP TABLE lexicon.shared_sentence_collections;
DROP TABLE lexicon.shared_sentence_annotations;
DROP TABLE lexicon.shared_sentences;
