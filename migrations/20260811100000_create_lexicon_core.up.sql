CREATE SCHEMA lexicon;
CREATE SCHEMA platform;

CREATE TABLE lexicon.entries (
    id UUID PRIMARY KEY,
    content_schema_version SMALLINT NOT NULL DEFAULT 2
        CONSTRAINT lexicon_entries_schema_version_check CHECK (content_schema_version = 2),
    language TEXT NOT NULL
        CONSTRAINT lexicon_entries_language_check CHECK (language = 'en'),
    kind TEXT NOT NULL
        CONSTRAINT lexicon_entries_kind_check CHECK (kind IN ('word', 'phrase')),
    revision BIGINT NOT NULL DEFAULT 1
        CONSTRAINT lexicon_entries_revision_check CHECK (revision > 0),
    headword_mode TEXT NOT NULL
        CONSTRAINT lexicon_entries_headword_mode_check CHECK (headword_mode IN ('unified', 'distinguish')),
    source_dialect TEXT
        CONSTRAINT lexicon_entries_source_dialect_check CHECK (source_dialect IN ('uk', 'us')),
    frequency NUMERIC(5, 2)
        CONSTRAINT lexicon_entries_frequency_check CHECK (frequency BETWEEN 0 AND 100),
    detection_snapshot JSONB NOT NULL,
    current_publication_id UUID,
    draft_based_on_publication_id UUID,
    created_by_admin_id UUID NOT NULL
        CONSTRAINT lexicon_entries_created_by_admin_fkey
        REFERENCES admins(id) ON DELETE RESTRICT,
    updated_by_admin_id UUID NOT NULL
        CONSTRAINT lexicon_entries_updated_by_admin_fkey
        REFERENCES admins(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    archived_at TIMESTAMPTZ,
    archived_by_admin_id UUID
        CONSTRAINT lexicon_entries_archived_by_admin_fkey
        REFERENCES admins(id) ON DELETE RESTRICT,
    CONSTRAINT lexicon_entries_headword_shape_check CHECK (
        (headword_mode = 'unified' AND source_dialect IS NULL)
        OR (headword_mode = 'distinguish' AND source_dialect IS NOT NULL)
    ),
    CONSTRAINT lexicon_entries_id_unique UNIQUE (id)
);

CREATE INDEX lexicon_entries_list_idx
    ON lexicon.entries (archived_at, updated_at DESC, id DESC);
CREATE INDEX lexicon_entries_creator_idx
    ON lexicon.entries (created_by_admin_id, updated_at DESC, id DESC);
CREATE INDEX lexicon_entries_current_publication_idx
    ON lexicon.entries (current_publication_id);

CREATE TABLE lexicon.entry_headwords (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL
        CONSTRAINT lexicon_entry_headwords_entry_fkey
        REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    dialect TEXT NOT NULL
        CONSTRAINT lexicon_entry_headwords_dialect_check CHECK (dialect IN ('common', 'uk', 'us')),
    headword TEXT NOT NULL,
    normalized_headword TEXT NOT NULL,
    normalization_version SMALLINT NOT NULL,
    origin TEXT NOT NULL
        CONSTRAINT lexicon_entry_headwords_origin_check CHECK (origin IN ('dictionary', 'converted', 'manual')),
    CONSTRAINT lexicon_entry_headwords_nonempty_check CHECK (
        headword = btrim(headword)
        AND normalized_headword = btrim(normalized_headword)
        AND char_length(headword) BETWEEN 1 AND 200
        AND char_length(normalized_headword) BETWEEN 1 AND 200
    ),
    CONSTRAINT lexicon_entry_headwords_entry_dialect_key UNIQUE (entry_id, dialect)
);

CREATE INDEX lexicon_entry_headwords_entry_idx ON lexicon.entry_headwords (entry_id);

CREATE TABLE lexicon.entry_headword_keys (
    entry_id UUID NOT NULL
        CONSTRAINT lexicon_entry_headword_keys_entry_fkey
        REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    language TEXT NOT NULL
        CONSTRAINT lexicon_entry_headword_keys_language_check CHECK (language = 'en'),
    kind TEXT NOT NULL
        CONSTRAINT lexicon_entry_headword_keys_kind_check CHECK (kind IN ('word', 'phrase')),
    dialect_scope TEXT NOT NULL
        CONSTRAINT lexicon_entry_headword_keys_dialect_check CHECK (dialect_scope IN ('uk', 'us')),
    normalized_headword TEXT NOT NULL,
    normalization_version SMALLINT NOT NULL,
    PRIMARY KEY (entry_id, dialect_scope),
    CONSTRAINT lexicon_entry_headword_keys_nonempty_check CHECK (
        normalized_headword = btrim(normalized_headword)
        AND char_length(normalized_headword) BETWEEN 1 AND 200
    )
);

CREATE UNIQUE INDEX lexicon_entry_headword_keys_unique_idx
    ON lexicon.entry_headword_keys (language, kind, dialect_scope, normalized_headword);

CREATE TABLE lexicon.nodes (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL
        CONSTRAINT lexicon_nodes_entry_fkey
        REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    node_type TEXT NOT NULL
        CONSTRAINT lexicon_nodes_type_check CHECK (node_type IN (
            'pos', 'form_group', 'form_slot', 'form_variant', 'pronunciation',
            'sense_group', 'grammar_structure', 'sense', 'definition', 'sentence',
            'text_variant', 'relation'
        )),
    first_published_at TIMESTAMPTZ,
    removed_from_draft_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT lexicon_nodes_id_entry_key UNIQUE (id, entry_id)
);

CREATE INDEX lexicon_nodes_entry_idx ON lexicon.nodes (entry_id, node_type);

CREATE TABLE lexicon.entry_step_progress (
    entry_id UUID NOT NULL
        CONSTRAINT lexicon_entry_step_progress_entry_fkey
        REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    step TEXT NOT NULL
        CONSTRAINT lexicon_entry_step_progress_step_check CHECK (step IN ('basics', 'forms', 'meanings')),
    completed_revision BIGINT NOT NULL
        CONSTRAINT lexicon_entry_step_progress_revision_check CHECK (completed_revision > 0),
    content_hash BYTEA NOT NULL,
    completed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (entry_id, step)
);

-- 管理端编辑器读模型。关系表仍是内容真相源；每次写事务同步重建本投影，
-- 避免 GET 为返回 canonical editor contract 执行几十次小查询。
CREATE TABLE lexicon.entry_editor_projection (
    entry_id UUID PRIMARY KEY
        CONSTRAINT lexicon_entry_editor_projection_entry_fkey
        REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    forms JSONB NOT NULL,
    meanings JSONB NOT NULL,
    rebuilt_revision BIGINT NOT NULL
        CONSTRAINT lexicon_entry_editor_projection_revision_check CHECK (rebuilt_revision > 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE platform.idempotency_records (
    scope TEXT NOT NULL,
    idempotency_key UUID NOT NULL,
    actor_id UUID NOT NULL,
    request_hash BYTEA NOT NULL,
    resource_id UUID,
    response_status SMALLINT NOT NULL,
    response_body JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (scope, actor_id, idempotency_key),
    CONSTRAINT platform_idempotency_response_status_check CHECK (response_status BETWEEN 200 AND 599),
    CONSTRAINT platform_idempotency_expiry_check CHECK (expires_at > created_at)
);

CREATE INDEX platform_idempotency_expiry_idx
    ON platform.idempotency_records (expires_at);
