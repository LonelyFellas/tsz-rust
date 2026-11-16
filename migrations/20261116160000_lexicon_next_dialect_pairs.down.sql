DO $$ BEGIN
    IF EXISTS (SELECT FROM lexicon_next.draft_revisions WHERE dialect_senses <> '[]'::jsonb)
       OR EXISTS (SELECT FROM lexicon_next.dialect_pairs) THEN
        RAISE EXCEPTION 'cannot discard authored dialect senses or historical pairs';
    END IF;
END $$;
ALTER TABLE lexicon_next.draft_revisions DROP COLUMN dialect_senses;
DROP TABLE lexicon_next.dialect_pairs;
