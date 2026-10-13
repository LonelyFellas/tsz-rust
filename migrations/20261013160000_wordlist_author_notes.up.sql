-- Public author annotations are separate from historical personal/admin notes.
ALTER TABLE wordlist_items ADD COLUMN author_note TEXT NOT NULL DEFAULT ''
    CHECK (char_length(author_note) <= 1000);
ALTER TABLE wordlist_review_requests ADD COLUMN author_notes JSONB NOT NULL DEFAULT '{}'::jsonb
    CHECK (jsonb_typeof(author_notes) = 'object');
