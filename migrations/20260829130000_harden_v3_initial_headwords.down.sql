ALTER TABLE lexicon.v3_entry_state
    DROP CONSTRAINT IF EXISTS lexicon_v3_entry_state_initial_headwords_shape_check,
    DROP COLUMN IF EXISTS initial_headword_keys;

ALTER TABLE lexicon.v3_entry_state
    ADD CONSTRAINT lexicon_v3_entry_state_initial_headwords_shape_check CHECK (
        initial_headwords IS NULL
        OR (
            initial_headwords ->> 'mode' = 'unified'
            AND jsonb_typeof(initial_headwords -> 'common') = 'string'
            AND btrim(initial_headwords ->> 'common') <> ''
            AND NOT initial_headwords ?| ARRAY['uk', 'us', 'source_dialect']
        )
        OR (
            initial_headwords ->> 'mode' = 'distinguish'
            AND jsonb_typeof(initial_headwords -> 'uk') = 'string'
            AND jsonb_typeof(initial_headwords -> 'us') = 'string'
            AND btrim(initial_headwords ->> 'uk') <> ''
            AND btrim(initial_headwords ->> 'us') <> ''
            AND initial_headwords ->> 'source_dialect' IN ('uk', 'us')
            AND NOT initial_headwords ? 'common'
        )
    );
