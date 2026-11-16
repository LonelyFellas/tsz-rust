-- Separate immutable publication documents; drafts and V1 remain independent.
ALTER TABLE lexicon_next.entries DROP CONSTRAINT entries_status_check;
ALTER TABLE lexicon_next.entries ADD CONSTRAINT entries_status_check CHECK (status IN ('draft','published','archived'));
CREATE TABLE lexicon_next.publications (
    id uuid PRIMARY KEY,
    entry_id uuid NOT NULL REFERENCES lexicon_next.entries(id),
    revision bigint NOT NULL,
    lifecycle_revision bigint NOT NULL CHECK (lifecycle_revision > 0),
    published_by_admin_id uuid NOT NULL REFERENCES admins(id),
    published_at timestamptz NOT NULL DEFAULT now(),
    snapshot jsonb NOT NULL,
    FOREIGN KEY (entry_id, revision) REFERENCES lexicon_next.draft_revisions(entry_id, revision),
    UNIQUE (entry_id, lifecycle_revision)
);
CREATE INDEX ON lexicon_next.publications(entry_id, lifecycle_revision DESC);
CREATE TABLE lexicon_next.publication_commands (
    actor_id uuid NOT NULL REFERENCES admins(id),
    request_id uuid NOT NULL,
    request jsonb NOT NULL,
    publication_id uuid NOT NULL REFERENCES lexicon_next.publications(id),
    PRIMARY KEY (actor_id, request_id)
);
CREATE TABLE lexicon_next.publication_audio_references (
    publication_id uuid NOT NULL REFERENCES lexicon_next.publications(id),
    asset_id uuid NOT NULL REFERENCES lexicon.audio_assets(id),
    resource_id uuid NOT NULL,
    PRIMARY KEY (publication_id, asset_id)
);
CREATE INDEX ON lexicon_next.publication_audio_references(asset_id);

-- Sentence drafts can later drop metadata; publications retain their own dependencies.
CREATE TABLE lexicon_next.publication_taxonomy_refs (
    publication_id uuid NOT NULL REFERENCES lexicon_next.publications(id),
    taxonomy_id uuid NOT NULL REFERENCES catalog.lexicon_taxonomies(id) ON DELETE RESTRICT,
    PRIMARY KEY (publication_id, taxonomy_id)
);
CREATE INDEX ON lexicon_next.publication_taxonomy_refs(taxonomy_id);

CREATE FUNCTION lexicon_next.reject_publication_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'lexicon-next publication snapshots are immutable';
END $$;
CREATE TRIGGER publications_immutable BEFORE UPDATE ON lexicon_next.publications
    FOR EACH ROW EXECUTE FUNCTION lexicon_next.reject_publication_update();
