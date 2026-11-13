-- Older binaries cannot read snapshots containing the new authoring fields.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon_next.nodes WHERE kind = 'sense_group')
       OR EXISTS (SELECT 1 FROM lexicon_next.draft_revisions WHERE document ? 'sense_groups') THEN
        RAISE EXCEPTION 'cannot remove lexicon-next sense groups while extended drafts exist';
    END IF;
END $$;
ALTER TABLE lexicon_next.nodes DROP CONSTRAINT nodes_kind_check;
ALTER TABLE lexicon_next.nodes ADD CONSTRAINT nodes_kind_check
    CHECK (kind IN ('form', 'sense', 'definition', 'equivalent'));
