-- Existing shared content has no publication boundary. Resetting test data requires
-- a separate, explicit environment-specific operation, not an implicit backfill.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.shared_sentences) THEN
        RAISE EXCEPTION 'shared sentence versioning requires an explicitly prepared empty shared-sentence dataset';
    END IF;
END $$;

ALTER TABLE lexicon.shared_sentences
    ADD COLUMN lifecycle_revision BIGINT NOT NULL DEFAULT 1 CHECK (lifecycle_revision > 0),
    ADD COLUMN current_publication_id UUID,
    ADD COLUMN withdrawn_at TIMESTAMPTZ,
    ADD COLUMN withdrawn_reason TEXT,
    ADD CONSTRAINT shared_sentence_withdrawn_shape CHECK (
        (withdrawn_at IS NULL AND withdrawn_reason IS NULL)
        OR (withdrawn_at IS NOT NULL AND length(btrim(withdrawn_reason)) BETWEEN 1 AND 2000)
    );

CREATE TABLE lexicon.shared_sentence_publications (
    id UUID PRIMARY KEY,
    sentence_id UUID NOT NULL REFERENCES lexicon.shared_sentences(id),
    publication_number BIGINT NOT NULL CHECK (publication_number > 0),
    source_revision BIGINT NOT NULL CHECK (source_revision > 0),
    snapshot JSONB NOT NULL CHECK (jsonb_typeof(snapshot) = 'object'),
    published_by_admin_id UUID NOT NULL REFERENCES admins(id),
    published_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    rollback_of_publication_id UUID,
    UNIQUE (id, sentence_id),
    UNIQUE (sentence_id, publication_number),
    FOREIGN KEY (rollback_of_publication_id, sentence_id)
        REFERENCES lexicon.shared_sentence_publications(id, sentence_id)
);
CREATE UNIQUE INDEX shared_sentence_regular_publication_revision
    ON lexicon.shared_sentence_publications(sentence_id, source_revision)
    WHERE rollback_of_publication_id IS NULL;
ALTER TABLE lexicon.shared_sentences
    ADD CONSTRAINT shared_sentence_current_publication_fkey
    FOREIGN KEY (current_publication_id, id)
    REFERENCES lexicon.shared_sentence_publications(id, sentence_id);

CREATE TABLE lexicon.shared_sentence_publication_annotations (
    publication_id UUID NOT NULL,
    sentence_id UUID NOT NULL,
    id UUID NOT NULL,
    source_dialect TEXT NOT NULL CHECK (source_dialect IN ('common', 'uk', 'us')),
    source_segments JSONB NOT NULL CHECK (jsonb_array_length(source_segments) BETWEEN 1 AND 20),
    target_entry_id UUID REFERENCES lexicon.entries(id),
    target_sense_id UUID,
    target_ref JSONB,
    pending_kind TEXT CHECK (pending_kind IN ('word', 'phrase')),
    pending_headword TEXT,
    pending_normalized TEXT,
    pending_gloss TEXT,
    PRIMARY KEY (publication_id, id),
    FOREIGN KEY (publication_id, sentence_id)
        REFERENCES lexicon.shared_sentence_publications(id, sentence_id),
    FOREIGN KEY (target_sense_id, target_entry_id) REFERENCES lexicon.nodes(id, entry_id),
    CHECK ((target_entry_id IS NOT NULL AND target_sense_id IS NOT NULL
            AND target_ref IS NOT NULL AND pending_kind IS NULL
            AND target_ref->>'state' = 'linked'
            AND target_ref->>'target_entry_id' = target_entry_id::text
            AND target_ref->>'target_sense_id' = target_sense_id::text
            AND target_ref->>'target_publication_id' IS NOT NULL)
        OR (target_entry_id IS NULL AND target_sense_id IS NULL AND target_ref IS NULL
            AND pending_kind IS NOT NULL AND pending_headword IS NOT NULL
            AND pending_normalized IS NOT NULL))
);
CREATE INDEX shared_sentence_publication_annotation_target
    ON lexicon.shared_sentence_publication_annotations(target_entry_id, target_sense_id);

CREATE TABLE lexicon.shared_sentence_draft_hides (
    entry_id UUID NOT NULL REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    sense_id UUID NOT NULL,
    sentence_id UUID NOT NULL REFERENCES lexicon.shared_sentences(id),
    PRIMARY KEY (entry_id, sense_id, sentence_id),
    FOREIGN KEY (sense_id, entry_id) REFERENCES lexicon.nodes(id, entry_id)
);
CREATE TABLE lexicon.shared_sentence_publication_hides (
    publication_id UUID NOT NULL,
    entry_id UUID NOT NULL,
    sense_id UUID NOT NULL,
    sentence_id UUID NOT NULL REFERENCES lexicon.shared_sentences(id),
    PRIMARY KEY (publication_id, sense_id, sentence_id),
    FOREIGN KEY (publication_id, entry_id) REFERENCES lexicon.entry_publications(id, entry_id),
    FOREIGN KEY (sense_id, entry_id) REFERENCES lexicon.nodes(id, entry_id)
);
CREATE INDEX shared_sentence_publication_hides_sentence
    ON lexicon.shared_sentence_publication_hides(sentence_id, entry_id);
