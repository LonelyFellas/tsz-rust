CREATE TABLE lexicon_next.test_resource_runs (
    run_id text PRIMARY KEY CHECK (run_id ~ '^[a-z0-9][a-z0-9_-]{7,79}$'),
    actor_id uuid NOT NULL REFERENCES admins(id),
    purge_request_id uuid,
    purge_request jsonb,
    CHECK ((purge_request_id IS NULL) = (purge_request IS NULL))
);

CREATE TABLE lexicon_next.test_resource_sentences (
    sentence_id uuid PRIMARY KEY REFERENCES lexicon_next.sentences(id) ON DELETE CASCADE,
    run_id text NOT NULL REFERENCES lexicon_next.test_resource_runs(run_id)
);
CREATE INDEX ON lexicon_next.test_resource_sentences(run_id);

ALTER TABLE lexicon.audio_generations ADD COLUMN test_run_id text
    REFERENCES lexicon_next.test_resource_runs(run_id);
CREATE INDEX ON lexicon.audio_generations(test_run_id) WHERE test_run_id IS NOT NULL;

CREATE TABLE lexicon_next.test_resource_previews (
    object_key text PRIMARY KEY,
    run_id text NOT NULL REFERENCES lexicon_next.test_resource_runs(run_id),
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32)
);
CREATE INDEX ON lexicon_next.test_resource_previews(run_id);

CREATE TABLE lexicon_next.test_resource_preview_requests (
    run_id text NOT NULL REFERENCES lexicon_next.test_resource_runs(run_id),
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    PRIMARY KEY (run_id, request_hash)
);
