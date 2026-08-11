DROP INDEX lexicon.lexicon_entries_archived_lifecycle_idx;

ALTER TABLE lexicon.entries
    DROP COLUMN lifecycle_revision;
