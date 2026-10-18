-- Deletion facts are independent of the live wordlist row. A later restore must not
-- erase the grace window already granted to in-flight learning runs.
CREATE TABLE wordlist_deletion_events (
    id UUID PRIMARY KEY,
    wordlist_id UUID NOT NULL,
    deleted_at TIMESTAMPTZ NOT NULL,
    grace_until TIMESTAMPTZ NOT NULL,
    prior_state TEXT NOT NULL CHECK (prior_state IN ('draft','pending','published','rejected','withdrawn')),
    public_generation BIGINT NOT NULL CHECK (public_generation>=0),
    deleted_by_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    deleted_by_admin_id UUID REFERENCES admins(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (deleted_by_user_id IS NULL OR deleted_by_admin_id IS NULL),
    CHECK (grace_until > deleted_at)
);

CREATE INDEX wordlist_deletion_events_lookup
    ON wordlist_deletion_events(wordlist_id, deleted_at DESC);

ALTER TABLE user_notifications DROP CONSTRAINT user_notifications_kind_check;
ALTER TABLE user_notifications
    ADD CONSTRAINT user_notifications_kind_check CHECK (kind IN (
        'teacher_approved', 'teacher_rejected', 'teacher_revoked',
        'wordlist_liked', 'wordlist_tipped', 'wordlist_deleted'
    ));
