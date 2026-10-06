CREATE TABLE coin_wallets (
    id UUID PRIMARY KEY,
    owner_type TEXT NOT NULL CHECK (owner_type IN ('user', 'admin')),
    owner_id UUID NOT NULL,
    balance BIGINT NOT NULL DEFAULT 0 CHECK (balance >= 0),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'deletion_pending', 'closed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (owner_type, owner_id),
    CHECK (status != 'closed' OR balance = 0)
);
-- Owner UUIDs deliberately survive physical account deletion; services lock and validate owners.
CREATE TABLE coin_operations (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('credit', 'debit', 'transfer', 'account_closure_forfeit')),
    actor_type TEXT NOT NULL CHECK (actor_type IN ('user', 'admin', 'system')),
    actor_id UUID,
    idempotency_scope TEXT NOT NULL CHECK (length(idempotency_scope) BETWEEN 1 AND 200),
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash) = 32),
    source_type TEXT NOT NULL CHECK (length(source_type) BETWEEN 1 AND 100),
    source_id TEXT NOT NULL CHECK (length(source_id) BETWEEN 1 AND 200),
    reason TEXT NOT NULL CHECK (length(reason) <= 1000),
    evidence_ref TEXT CHECK (length(evidence_ref) BETWEEN 1 AND 500),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK ((actor_type = 'system') = (actor_id IS NULL)),
    UNIQUE (idempotency_scope, idempotency_key),
    UNIQUE (source_type, source_id)
);
CREATE TABLE coin_entries (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    operation_id UUID NOT NULL REFERENCES coin_operations(id) ON DELETE RESTRICT,
    wallet_id UUID NOT NULL REFERENCES coin_wallets(id) ON DELETE RESTRICT,
    delta BIGINT NOT NULL CHECK (delta != 0),
    balance_after BIGINT NOT NULL CHECK (balance_after >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (operation_id, wallet_id)
);
CREATE INDEX coin_entries_wallet_page ON coin_entries (wallet_id, id DESC);
