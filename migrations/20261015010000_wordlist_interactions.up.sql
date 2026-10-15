CREATE TABLE wordlist_interactions (
    wordlist_id uuid NOT NULL REFERENCES wordlists(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('like', 'favorite')),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (wordlist_id, user_id, kind)
);
CREATE INDEX wordlist_interactions_user_favorites_idx
    ON wordlist_interactions(user_id, created_at DESC, wordlist_id) WHERE kind = 'favorite';
