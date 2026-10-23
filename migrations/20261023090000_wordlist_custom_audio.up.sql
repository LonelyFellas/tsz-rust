CREATE TABLE wordlist_audio_jobs (
    id uuid PRIMARY KEY,
    wordlist_id uuid NOT NULL,
    item_id uuid NOT NULL,
    user_id uuid NOT NULL,
    word text NOT NULL CHECK (char_length(word) BETWEEN 1 AND 50),
    voices jsonb NOT NULL CHECK (jsonb_array_length(voices) = 2),
    state text NOT NULL CHECK (state IN ('queued','processing','ready','refunding','refunded')),
    is_current boolean NOT NULL DEFAULT true,
    charge_operation_id uuid NOT NULL REFERENCES coin_operations(id),
    refund_operation_id uuid REFERENCES coin_operations(id),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    lease_token uuid,
    lease_until timestamptz,
    test_run text,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CHECK ((state = 'refunded') = (refund_operation_id IS NOT NULL))
);
CREATE UNIQUE INDEX wordlist_audio_current ON wordlist_audio_jobs(wordlist_id,item_id) WHERE is_current;
CREATE INDEX wordlist_audio_work ON wordlist_audio_jobs(next_attempt_at) WHERE state IN ('queued','processing','refunding');
CREATE INDEX wordlist_audio_user_unsettled ON wordlist_audio_jobs(user_id) WHERE state IN ('queued','processing','refunding');

CREATE TABLE wordlist_audio_objects (
    job_id uuid NOT NULL REFERENCES wordlist_audio_jobs(id),
    attempt_token uuid NOT NULL,
    locale text NOT NULL CHECK (locale IN ('en-GB','en-US')),
    object_key text NOT NULL UNIQUE,
    uploaded boolean NOT NULL DEFAULT false,
    cleanup_after timestamptz NOT NULL,
    deleted_at timestamptz,
    PRIMARY KEY(job_id,attempt_token,locale)
);

CREATE TABLE wordlist_audio_review_refs (
    review_id uuid NOT NULL REFERENCES wordlist_review_requests(id) ON DELETE CASCADE,
    item_id uuid NOT NULL,
    job_id uuid NOT NULL REFERENCES wordlist_audio_jobs(id),
    PRIMARY KEY(review_id,item_id)
);

CREATE TABLE wordlist_update_requests (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    request_key uuid NOT NULL,
    wordlist_id uuid NOT NULL REFERENCES wordlists(id) ON DELETE CASCADE,
    request_hash bytea NOT NULL,
    response jsonb NOT NULL,
    PRIMARY KEY(user_id,request_key)
);
