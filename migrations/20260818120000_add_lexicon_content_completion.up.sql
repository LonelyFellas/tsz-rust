CREATE TABLE lexicon.content_completion_jobs (
    id UUID PRIMARY KEY,
    entry_id UUID NOT NULL REFERENCES lexicon.entries(id) ON DELETE CASCADE,
    actor_id UUID NOT NULL REFERENCES admins(id),
    idempotency_key UUID NOT NULL,
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    base_revision BIGINT NOT NULL CHECK (base_revision > 0),
    requested_scope TEXT[] NOT NULL,
    fill_policy TEXT NOT NULL CHECK (fill_policy = 'missing_only'),
    source_snapshot JSONB NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'partial', 'failed')),
    result JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (actor_id, idempotency_key)
);

CREATE INDEX content_completion_jobs_entry_idx
ON lexicon.content_completion_jobs (entry_id, created_at DESC);

CREATE INDEX content_completion_jobs_pending_idx
ON lexicon.content_completion_jobs (updated_at, id)
WHERE status IN ('pending', 'running');

CREATE TABLE lexicon.content_completion_partitions (
    job_id UUID NOT NULL REFERENCES lexicon.content_completion_jobs(id) ON DELETE CASCADE,
    pos_id UUID NOT NULL,
    pos TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'missing', 'failed')),
    attempt INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    error_code TEXT,
    error_detail TEXT,
    result JSONB,
    provenance JSONB,
    lease_expires_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (job_id, pos_id)
);

CREATE INDEX content_completion_partitions_claim_idx
ON lexicon.content_completion_partitions (updated_at, job_id, pos_id)
WHERE status IN ('pending', 'running');

-- 完整 Kaikki 内容独立导入，避免扩大词头检测使用的轻量 active_terms 读模型。
CREATE TABLE dictionary.content_imports (
    dataset_id BIGINT PRIMARY KEY REFERENCES dictionary.datasets(id) ON DELETE CASCADE,
    input_sha256 TEXT NOT NULL CHECK (input_sha256 ~ '^[0-9a-f]{64}$'),
    source_locator TEXT NOT NULL CHECK (char_length(btrim(source_locator)) > 0),
    record_count BIGINT NOT NULL CHECK (record_count >= 0),
    imported_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE dictionary.entry_contents (
    dataset_id BIGINT NOT NULL REFERENCES dictionary.datasets(id) ON DELETE CASCADE,
    source_key TEXT NOT NULL,
    normalized_term TEXT NOT NULL,
    pos TEXT NOT NULL,
    senses JSONB NOT NULL CHECK (jsonb_typeof(senses) = 'array'),
    source_locator TEXT NOT NULL,
    PRIMARY KEY (dataset_id, source_key)
);

CREATE INDEX dictionary_entry_contents_lookup_idx
ON dictionary.entry_contents (normalized_term, pos, dataset_id);
