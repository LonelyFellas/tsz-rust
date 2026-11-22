CREATE TABLE lexicon_next.test_resource_entries (
    run_id text NOT NULL
        CHECK (run_id ~ '^[a-z0-9][a-z0-9_-]{7,79}$'),
    entry_id uuid PRIMARY KEY REFERENCES lexicon_next.entries(id) ON DELETE CASCADE,
    actor_id uuid NOT NULL REFERENCES admins(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX lexicon_next_test_resource_entries_run_idx
    ON lexicon_next.test_resource_entries(run_id, entry_id);

CREATE TABLE lexicon_next.test_resource_purge_commands (
    actor_id uuid NOT NULL REFERENCES admins(id) ON DELETE RESTRICT,
    request_id uuid NOT NULL,
    run_id text NOT NULL
        CHECK (run_id ~ '^[a-z0-9][a-z0-9_-]{7,79}$'),
    request jsonb NOT NULL CHECK (jsonb_typeof(request) = 'object'),
    result jsonb NOT NULL CHECK (jsonb_typeof(result) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (actor_id, request_id)
);
