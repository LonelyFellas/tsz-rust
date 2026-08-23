ALTER TABLE lexicon.entry_publication_sense_refs
    ADD COLUMN target_content_scope TEXT,
    ADD COLUMN target_revision BIGINT;

UPDATE lexicon.entry_publication_sense_refs sense_ref
SET target_content_scope = 'publication',
    target_revision = publication.source_revision
FROM lexicon.entry_publications publication
WHERE publication.id = sense_ref.target_publication_id
  AND publication.entry_id = sense_ref.target_entry_id;

ALTER TABLE lexicon.entry_publications
    ADD CONSTRAINT lexicon_entry_publications_id_entry_revision_key
        UNIQUE (id, entry_id, source_revision);

ALTER TABLE lexicon.entry_publication_sense_refs
    ALTER COLUMN target_content_scope SET NOT NULL,
    ALTER COLUMN target_revision SET NOT NULL,
    ALTER COLUMN target_publication_id DROP NOT NULL,
    ADD CONSTRAINT lexicon_publication_sense_refs_target_scope_check
        CHECK (
            (target_content_scope = 'draft' AND target_publication_id IS NULL)
            OR
            (target_content_scope = 'publication' AND target_publication_id IS NOT NULL)
        ),
    ADD CONSTRAINT lexicon_publication_sense_refs_target_revision_check
        CHECK (target_revision > 0),
    ADD CONSTRAINT lexicon_publication_sense_refs_context_target_check
        CHECK (
            reference_kind = 'relation'
            OR target_content_scope = 'publication'
        ),
    ADD CONSTRAINT lexicon_publication_sense_refs_target_node_fkey
        FOREIGN KEY (target_sense_id, target_entry_id)
        REFERENCES lexicon.nodes(id, entry_id) ON DELETE RESTRICT,
    ADD CONSTRAINT lexicon_publication_sense_refs_target_revision_fkey
        FOREIGN KEY (target_publication_id, target_entry_id, target_revision)
        REFERENCES lexicon.entry_publications(id, entry_id, source_revision)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE;

COMMENT ON COLUMN lexicon.entry_publication_sense_refs.target_content_scope IS
    '目标快照在来源发布时取自 draft 或目标 current publication；历史行不可随目标后续发布改写。';
COMMENT ON COLUMN lexicon.entry_publication_sense_refs.target_revision IS
    '来源发布时用于规范化 target_headword/target_gloss 的目标 entry revision 或 publication source_revision。';
