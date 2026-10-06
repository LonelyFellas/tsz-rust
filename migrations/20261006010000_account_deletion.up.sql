CREATE TABLE account_deletion_requests (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending','cancelled','completed')),
    requested_at TIMESTAMPTZ NOT NULL,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    effective_at TIMESTAMPTZ NOT NULL,
    cancelled_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    confirmed_balance BIGINT NOT NULL CHECK (confirmed_balance >= 0),
    waive_balance BOOLEAN NOT NULL,
    confirm_deletion BOOLEAN NOT NULL CHECK (confirm_deletion),
    consent_version TEXT NOT NULL,
    consent_text TEXT NOT NULL CHECK (length(consent_text)>0),
    signed_at TIMESTAMPTZ NOT NULL,
    verification_channel TEXT NOT NULL CHECK (verification_channel IN ('phone','email')),
    idempotency_key UUID NOT NULL,
    request_hash BYTEA NOT NULL CHECK (octet_length(request_hash)=32),
    UNIQUE (user_id,idempotency_key),
    CHECK (confirmed_balance=0 OR waive_balance),
    CHECK (effective_at=requested_at+interval '72 hours'),
    CHECK (signed_at=requested_at),
    CHECK ((status='pending' AND cancelled_at IS NULL AND completed_at IS NULL)
        OR (status='cancelled' AND cancelled_at IS NOT NULL AND cancelled_at<effective_at AND completed_at IS NULL)
        OR (status='completed' AND completed_at IS NOT NULL AND completed_at>=effective_at AND cancelled_at IS NULL))
);
-- No account FK: retain consent and execution evidence after physical deletion.
CREATE UNIQUE INDEX account_deletion_one_pending ON account_deletion_requests(user_id) WHERE status='pending';
CREATE INDEX account_deletion_due ON account_deletion_requests(next_attempt_at,effective_at,id) WHERE status='pending';
CREATE INDEX account_deletion_history ON account_deletion_requests(user_id,requested_at DESC);
