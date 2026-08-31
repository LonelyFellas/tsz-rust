ALTER TABLE lexicon.v3_entry_state
    DROP CONSTRAINT IF EXISTS lexicon_v3_entry_state_initial_headwords_shape_check,
    DROP COLUMN IF EXISTS initial_headword_keys,
    DROP COLUMN IF EXISTS initial_headwords;
