-- Names, ordering and sense membership live in the existing immutable draft snapshots.
-- Register group identities alongside the other retained lexicon-next nodes.
ALTER TABLE lexicon_next.nodes DROP CONSTRAINT nodes_kind_check;
ALTER TABLE lexicon_next.nodes ADD CONSTRAINT nodes_kind_check
    CHECK (kind IN ('form', 'sense', 'definition', 'equivalent', 'sense_group'));
