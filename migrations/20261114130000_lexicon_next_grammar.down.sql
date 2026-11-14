DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon_next.nodes WHERE kind IN ('grammar', 'grammar_en', 'grammar_en_gb', 'grammar_en_us'))
       OR EXISTS (SELECT 1 FROM lexicon_next.draft_revisions WHERE document ? 'grammar_structures') THEN
        RAISE EXCEPTION 'cannot remove lexicon-next grammar while extended drafts exist';
    END IF;
END $$;
ALTER TABLE lexicon_next.nodes DROP CONSTRAINT nodes_kind_check;
ALTER TABLE lexicon_next.nodes ADD CONSTRAINT nodes_kind_check
    CHECK (kind IN ('form', 'sense', 'definition', 'equivalent', 'sense_group'));
