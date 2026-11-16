-- The endpoint identity never changes. Draft revisions on both sides name the
-- same pair; published entries retain the exact identities they approved.
CREATE TABLE lexicon_next.dialect_pairs (
    id uuid PRIMARY KEY,
    gb_entry uuid NOT NULL, gb_sense uuid NOT NULL,
    us_entry uuid NOT NULL, us_sense uuid NOT NULL,
    CHECK (gb_entry <> us_entry AND gb_sense <> us_sense)
);
ALTER TABLE lexicon_next.draft_revisions ADD COLUMN dialect_senses jsonb NOT NULL DEFAULT '[]'
    CHECK (jsonb_typeof(dialect_senses) = 'array');
