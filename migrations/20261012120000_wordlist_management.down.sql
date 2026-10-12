-- Reject destructive rollback: platform authors, deleted rows and independent review
-- states cannot be represented faithfully by the previous model.
DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM admin_permission_grants WHERE permission_key IN ('wordlists.create','wordlists.edit','wordlists.visibility','wordlists.recommend','wordlists.delete')) OR EXISTS(SELECT 1 FROM permission_tag_items WHERE permission_key IN ('wordlists.create','wordlists.edit','wordlists.visibility','wordlists.recommend','wordlists.delete')) THEN
        RAISE EXCEPTION 'wordlist management permissions require explicit migration before rollback';
    END IF;
    IF EXISTS(SELECT 1 FROM wordlists WHERE owner_admin_id IS NOT NULL OR deleted_at IS NOT NULL OR recommended OR (state='published' AND review_state<>'approved') OR (state='draft' AND review_state<>'none')) THEN
        RAISE EXCEPTION 'wordlist management data requires explicit migration before rollback';
    END IF;
END $$;
CREATE OR REPLACE FUNCTION wordlists_preserve_owner() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.owner_user_id IS DISTINCT FROM OLD.owner_user_id OR NEW.create_key IS DISTINCT FROM OLD.create_key OR NEW.create_hash IS DISTINCT FROM OLD.create_hash THEN
        RAISE EXCEPTION 'wordlist owner is immutable';
    END IF;
    RETURN NEW;
END $$;
ALTER TABLE wordlist_items DROP COLUMN admin_note;
ALTER TABLE wordlists DROP CONSTRAINT wordlists_recommend_public;
DROP INDEX wordlists_recommended_idx;
DROP INDEX wordlists_admin_create_key;
ALTER TABLE wordlists DROP CONSTRAINT wordlists_one_owner;
ALTER TABLE wordlists DROP COLUMN owner_admin_id, DROP COLUMN review_state, DROP COLUMN recommended, DROP COLUMN deleted_at;
ALTER TABLE wordlists ALTER COLUMN owner_user_id SET NOT NULL;
