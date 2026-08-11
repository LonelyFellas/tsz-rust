ALTER TABLE lexicon.entries
    ADD COLUMN lifecycle_revision BIGINT NOT NULL DEFAULT 1
        CONSTRAINT lexicon_entries_lifecycle_revision_check CHECK (lifecycle_revision > 0);

CREATE INDEX lexicon_entries_archived_lifecycle_idx
    ON lexicon.entries (archived_at, lifecycle_revision, id);
