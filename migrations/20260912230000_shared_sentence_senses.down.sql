DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.shared_sentence_sense_collections) THEN
        RAISE EXCEPTION 'Refusing to discard shared sentence sense collections';
    END IF;
END $$;
DROP TABLE lexicon.shared_sentence_sense_collections;
