DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM lexicon_next.sentences s,
            jsonb_array_elements(s.document->'variants') v
        WHERE v ? 'metadata' OR EXISTS (
            SELECT 1 FROM jsonb_array_elements(v->'translations') t WHERE t ? 'metadata'
        )
    ) THEN RAISE EXCEPTION 'sentence details exist; a lossless rollback is required'; END IF;
END $$;
DROP TABLE lexicon_next.sentence_taxonomy_refs;
