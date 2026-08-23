DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM lexicon.entry_publication_sense_refs
        WHERE target_publication_id IS NULL
    ) THEN
        RAISE EXCEPTION
            'cannot restore target_publication_id NOT NULL while draft-target publication references exist';
    END IF;
END
$$;

ALTER TABLE lexicon.entry_publication_sense_refs
    DROP CONSTRAINT lexicon_publication_sense_refs_target_revision_fkey,
    DROP CONSTRAINT lexicon_publication_sense_refs_target_node_fkey,
    DROP CONSTRAINT lexicon_publication_sense_refs_context_target_check,
    DROP CONSTRAINT lexicon_publication_sense_refs_target_revision_check,
    DROP CONSTRAINT lexicon_publication_sense_refs_target_scope_check,
    ALTER COLUMN target_publication_id SET NOT NULL,
    DROP COLUMN target_revision,
    DROP COLUMN target_content_scope;

ALTER TABLE lexicon.entry_publications
    DROP CONSTRAINT lexicon_entry_publications_id_entry_revision_key;
