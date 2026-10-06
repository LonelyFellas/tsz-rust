-- Publication updates also increment lifecycle_revision. Track archive transitions separately.
ALTER TABLE lexicon.entries ADD COLUMN wordlist_archive_generation BIGINT NOT NULL DEFAULT 0 CHECK(wordlist_archive_generation>=0);
CREATE FUNCTION advance_wordlist_archive_generation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.archived_at IS NULL AND NEW.archived_at IS NOT NULL THEN
        NEW.wordlist_archive_generation := OLD.wordlist_archive_generation + 1;
    ELSE
        NEW.wordlist_archive_generation := OLD.wordlist_archive_generation;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER wordlist_archive_generation BEFORE UPDATE ON lexicon.entries
    FOR EACH ROW EXECUTE FUNCTION advance_wordlist_archive_generation();
ALTER TABLE wordlist_items ADD COLUMN approved_archive_generation BIGINT;
-- Preserve an approval only when no lifecycle change occurred since it. Otherwise require re-review.
UPDATE wordlist_items i SET approved_archive_generation=0 FROM lexicon.entries e,wordlists w
WHERE i.entry_id=e.id AND i.wordlist_id=w.id AND w.state='published' AND e.archived_at IS NULL
    AND i.approved_lifecycle_revision=e.lifecycle_revision;
CREATE TABLE wordlist_tips (
    event_id UUID PRIMARY KEY,
    wordlist_id UUID NOT NULL,
    payer_user_id UUID NOT NULL,
    author_user_id UUID NOT NULL,
    amount BIGINT NOT NULL CHECK(amount>0),
    request_key UUID NOT NULL,
    request_hash BYTEA NOT NULL,
    operation_id UUID NOT NULL UNIQUE REFERENCES coin_operations(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK(payer_user_id<>author_user_id),
    UNIQUE(payer_user_id,request_key)
);
CREATE INDEX wordlist_tips_payer ON wordlist_tips(payer_user_id,created_at DESC,event_id DESC);
CREATE INDEX wordlist_tips_author ON wordlist_tips(author_user_id,created_at DESC,event_id DESC);
CREATE FUNCTION preserve_wordlist_tip() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'wordlist tip facts are immutable'; END $$;
CREATE TRIGGER wordlist_tip_immutable BEFORE UPDATE OR DELETE ON wordlist_tips
    FOR EACH ROW EXECUTE FUNCTION preserve_wordlist_tip();
