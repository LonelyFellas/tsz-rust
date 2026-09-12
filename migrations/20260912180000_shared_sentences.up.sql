-- Current shared content is independent from immutable word publication snapshots.
CREATE TABLE lexicon.shared_sentences (
    id UUID PRIMARY KEY,
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    content JSONB NOT NULL,
    create_digest TEXT NOT NULL,
    source_entry_id UUID REFERENCES lexicon.entries(id) ON DELETE SET NULL,
    created_by_admin_id UUID NOT NULL REFERENCES admins(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ
);
CREATE TABLE lexicon.shared_sentence_annotations (
    sentence_id UUID NOT NULL REFERENCES lexicon.shared_sentences(id) ON DELETE CASCADE,
    id UUID NOT NULL,
    source_dialect TEXT NOT NULL CHECK (source_dialect IN ('common','uk','us')),
    source_segments JSONB NOT NULL CHECK (jsonb_array_length(source_segments) BETWEEN 1 AND 20),
    target_entry_id UUID REFERENCES lexicon.entries(id) ON DELETE RESTRICT,
    pending_kind TEXT CHECK (pending_kind IN ('word','phrase')),
    pending_headword TEXT,
    pending_normalized TEXT,
    pending_gloss TEXT,
    PRIMARY KEY(sentence_id,id),
    CHECK ((target_entry_id IS NOT NULL AND pending_kind IS NULL AND pending_headword IS NULL AND pending_normalized IS NULL AND pending_gloss IS NULL)
        OR (target_entry_id IS NULL AND pending_kind IS NOT NULL AND length(pending_headword) BETWEEN 1 AND 200 AND pending_normalized IS NOT NULL))
);
CREATE INDEX shared_sentence_target_idx ON lexicon.shared_sentence_annotations(target_entry_id);
CREATE INDEX shared_sentence_pending_idx ON lexicon.shared_sentence_annotations(pending_kind,pending_normalized) WHERE target_entry_id IS NULL;
CREATE TABLE lexicon.shared_sentence_collections (
    sentence_id UUID NOT NULL REFERENCES lexicon.shared_sentences(id) ON DELETE CASCADE,
    entry_id UUID NOT NULL REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(sentence_id,entry_id)
);
CREATE INDEX shared_sentence_collection_entry_idx ON lexicon.shared_sentence_collections(entry_id);
CREATE INDEX shared_sentence_list_idx ON lexicon.shared_sentences(created_at DESC,id) WHERE deleted_at IS NULL;
