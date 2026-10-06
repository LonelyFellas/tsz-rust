CREATE TABLE wordlists (
    id UUID PRIMARY KEY,
    owner_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (char_length(btrim(name)) BETWEEN 1 AND 100),
    state TEXT NOT NULL DEFAULT 'draft' CHECK (state IN ('draft','pending','published','rejected','withdrawn')),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    withdraw_reason TEXT,
    create_key UUID NOT NULL,
    create_hash BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE(owner_user_id,create_key)
);
CREATE INDEX wordlists_owner_idx ON wordlists(owner_user_id,created_at DESC,id);
CREATE INDEX wordlists_public_idx ON wordlists(created_at DESC,id) WHERE state='published';

CREATE FUNCTION wordlists_preserve_owner() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.owner_user_id IS DISTINCT FROM OLD.owner_user_id OR NEW.create_key IS DISTINCT FROM OLD.create_key OR NEW.create_hash IS DISTINCT FROM OLD.create_hash THEN
        RAISE EXCEPTION 'wordlist owner is immutable';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER wordlists_owner_immutable BEFORE UPDATE ON wordlists
    FOR EACH ROW EXECUTE FUNCTION wordlists_preserve_owner();

CREATE TABLE wordlist_items (
    wordlist_id UUID NOT NULL REFERENCES wordlists(id) ON DELETE CASCADE,
    entry_id UUID NOT NULL REFERENCES lexicon.entries(id) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK (position BETWEEN 0 AND 9999),
    approved_lifecycle_revision BIGINT,
    private_note TEXT NOT NULL DEFAULT '' CHECK (char_length(private_note)<=1000),
    note_revision BIGINT NOT NULL DEFAULT 1 CHECK (note_revision>0),
    PRIMARY KEY(wordlist_id,entry_id),
    UNIQUE(wordlist_id,position) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE wordlist_review_requests (
    id UUID PRIMARY KEY,
    wordlist_id UUID NOT NULL REFERENCES wordlists(id) ON DELETE CASCADE,
    submitted_revision BIGINT NOT NULL CHECK (submitted_revision>0),
    name TEXT NOT NULL,
    entry_ids UUID[] NOT NULL CHECK (cardinality(entry_ids) BETWEEN 1 AND 10000),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','approved','rejected','cancelled')),
    submit_key UUID NOT NULL,
    request_hash BYTEA NOT NULL,
    reviewer_id UUID,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    decided_at TIMESTAMPTZ,
    UNIQUE(wordlist_id,submit_key)
);
CREATE UNIQUE INDEX wordlist_review_pending_once ON wordlist_review_requests(wordlist_id) WHERE state='pending';
