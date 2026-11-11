-- Destructive: removes all new-library drafts/history. Back up before rollback.
-- Existing lexicon and dictionary schemas are untouched.
DROP SCHEMA lexicon_next CASCADE;
