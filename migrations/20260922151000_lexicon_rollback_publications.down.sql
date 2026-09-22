DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.entry_publications WHERE rollback_of_publication_id IS NOT NULL) THEN
        RAISE EXCEPTION 'cannot revert while rollback publications exist; publication history must be retained';
    END IF;
END $$;

DROP INDEX lexicon.lexicon_publications_draft_revision_key;
ALTER TABLE lexicon.entry_publications
    DROP CONSTRAINT lexicon_publication_rollback_source_fkey,
    DROP COLUMN rollback_of_publication_id,
    ADD CONSTRAINT lexicon_entry_publications_entry_schema_revision_key
        UNIQUE (entry_id, content_schema_version, source_revision);
