DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM lexicon_next.sentences)
        OR EXISTS(SELECT 1 FROM lexicon_next.sentence_references)
    THEN RAISE EXCEPTION 'lexicon-next sentence data exists; rollback would lose data'; END IF;
END $$;
DROP TABLE lexicon_next.sentence_audio_asset_references;
DROP TABLE lexicon_next.sentence_references;
DROP TABLE lexicon_next.sentence_nodes;
DROP TABLE lexicon_next.sentences;
