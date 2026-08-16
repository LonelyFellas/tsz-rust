-- B4 reviewed cutover artifact. This file is intentionally outside migrations/:
-- application startup must never execute this irreversible step automatically.
DROP INDEX lexicon.lexicon_entry_headword_keys_unique_idx;
