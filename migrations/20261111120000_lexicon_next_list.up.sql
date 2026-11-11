-- List metadata is separate from language drafts; existing draft wire formats stay unchanged.
ALTER TABLE lexicon_next.entries
    ADD COLUMN created_at timestamptz,
    ADD COLUMN status text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'archived')),
    ADD COLUMN lifecycle_revision bigint NOT NULL DEFAULT 1 CHECK (lifecycle_revision > 0),
    ADD COLUMN annotation text CHECK (char_length(annotation) BETWEEN 1 AND 20),
    ADD COLUMN annotation_revision bigint NOT NULL DEFAULT 1 CHECK (annotation_revision > 0);
UPDATE lexicon_next.entries e SET created_at = COALESCE(
    (SELECT min(r.created_at) FROM lexicon_next.draft_revisions r WHERE r.entry_id=e.id), e.updated_at
);
ALTER TABLE lexicon_next.entries ALTER COLUMN created_at SET NOT NULL;
ALTER TABLE lexicon_next.entries ALTER COLUMN created_at SET DEFAULT now();
CREATE INDEX ON lexicon_next.entries(status, created_at DESC, id DESC);
-- Retain only command receipts, allowing retries after a permanent deletion.
CREATE TABLE lexicon_next.list_commands (
    actor_id uuid NOT NULL REFERENCES admins(id),
    request_id uuid NOT NULL,
    request jsonb NOT NULL,
    result jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(actor_id, request_id)
);
