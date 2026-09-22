ALTER TABLE lexicon.entry_publications
    ADD COLUMN rollback_of_publication_id UUID,
    ADD CONSTRAINT lexicon_publication_rollback_source_fkey
        FOREIGN KEY (rollback_of_publication_id, entry_id)
        REFERENCES lexicon.entry_publications(id, entry_id) ON DELETE RESTRICT,
    DROP CONSTRAINT lexicon_entry_publications_entry_schema_revision_key;

CREATE UNIQUE INDEX lexicon_publications_draft_revision_key
    ON lexicon.entry_publications(entry_id, content_schema_version, source_revision)
    WHERE rollback_of_publication_id IS NULL;
