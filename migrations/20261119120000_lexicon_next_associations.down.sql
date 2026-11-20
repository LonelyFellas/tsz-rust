DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon_next.grammar_form_links)
       OR EXISTS (SELECT 1 FROM lexicon_next.sentence_associations)
    THEN RAISE EXCEPTION 'lexicon-next association data exists; rollback would lose data'; END IF;
END $$;
DROP TABLE lexicon_next.sentence_associations;
DROP TABLE lexicon_next.grammar_form_links;
