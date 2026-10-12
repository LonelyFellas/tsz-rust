ALTER TABLE wordlists ALTER COLUMN owner_user_id DROP NOT NULL;
ALTER TABLE wordlists ADD COLUMN owner_admin_id UUID REFERENCES admins(id) ON DELETE RESTRICT;
ALTER TABLE wordlists ADD CONSTRAINT wordlists_one_owner CHECK ((owner_user_id IS NULL) <> (owner_admin_id IS NULL));
CREATE UNIQUE INDEX wordlists_admin_create_key ON wordlists(owner_admin_id,create_key) WHERE owner_admin_id IS NOT NULL;
ALTER TABLE wordlists ADD COLUMN review_state TEXT NOT NULL DEFAULT 'none' CHECK (review_state IN ('none','pending','approved','rejected'));
UPDATE wordlists w SET review_state=CASE WHEN w.state='draft' THEN 'none' ELSE COALESCE((SELECT CASE WHEN r.state='cancelled' THEN 'none' ELSE r.state END FROM wordlist_review_requests r WHERE r.wordlist_id=w.id ORDER BY r.created_at DESC,r.id DESC LIMIT 1),'none') END;
ALTER TABLE wordlists ADD COLUMN recommended BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE wordlists ADD COLUMN deleted_at TIMESTAMPTZ;
ALTER TABLE wordlist_items ADD COLUMN admin_note TEXT NOT NULL DEFAULT '' CHECK (char_length(admin_note)<=1000);
ALTER TABLE wordlists ADD CONSTRAINT wordlists_recommend_public CHECK (NOT recommended OR (state='published' AND deleted_at IS NULL));
CREATE INDEX wordlists_recommended_idx ON wordlists(created_at DESC,id) WHERE recommended AND deleted_at IS NULL;
CREATE OR REPLACE FUNCTION wordlists_preserve_owner() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.owner_user_id IS DISTINCT FROM OLD.owner_user_id OR NEW.owner_admin_id IS DISTINCT FROM OLD.owner_admin_id OR NEW.create_key IS DISTINCT FROM OLD.create_key OR NEW.create_hash IS DISTINCT FROM OLD.create_hash THEN
        RAISE EXCEPTION 'wordlist owner is immutable';
    END IF;
    IF NEW.state <> 'published' OR NEW.deleted_at IS NOT NULL THEN
        NEW.recommended := false;
    END IF;
    RETURN NEW;
END $$;
