ALTER TABLE lexicon_next.equivalents ADD COLUMN metadata jsonb;

-- Retained revisions keep their taxonomy references until the entry is permanently deleted.
CREATE TABLE lexicon_next.equivalent_taxonomy_refs (
    entry_id uuid NOT NULL,
    revision bigint NOT NULL,
    taxonomy_id uuid NOT NULL REFERENCES catalog.lexicon_taxonomies(id) ON DELETE RESTRICT,
    PRIMARY KEY (entry_id, revision, taxonomy_id),
    FOREIGN KEY (entry_id, revision) REFERENCES lexicon_next.draft_revisions(entry_id, revision) ON DELETE CASCADE
);
CREATE INDEX equivalent_taxonomy_refs_taxonomy ON lexicon_next.equivalent_taxonomy_refs(taxonomy_id);
