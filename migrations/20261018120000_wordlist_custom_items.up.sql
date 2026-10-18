ALTER TABLE wordlist_items ADD COLUMN item_id uuid;
UPDATE wordlist_items SET item_id=entry_id;
ALTER TABLE wordlist_items ALTER COLUMN item_id SET NOT NULL;
ALTER TABLE wordlist_items DROP CONSTRAINT wordlist_items_pkey;
ALTER TABLE wordlist_items ALTER COLUMN entry_id DROP NOT NULL;
ALTER TABLE wordlist_items ADD PRIMARY KEY(wordlist_id,item_id);
ALTER TABLE wordlist_items ADD UNIQUE(wordlist_id,entry_id);
ALTER TABLE wordlist_items ADD COLUMN custom_word jsonb;
ALTER TABLE wordlist_items ADD CONSTRAINT wordlist_item_source CHECK (
    ((entry_id IS NOT NULL AND custom_word IS NULL) OR
     (entry_id IS NULL AND jsonb_typeof(custom_word)='object'
      AND jsonb_typeof(custom_word->'word')='string'
      AND jsonb_typeof(custom_word->'definition')='string'
      AND char_length(btrim(custom_word->>'word')) BETWEEN 1 AND 50
      AND char_length(btrim(custom_word->>'definition')) BETWEEN 1 AND 200)) IS TRUE
);
CREATE FUNCTION wordlist_item_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='INSERT' AND NEW.entry_id IS NOT NULL THEN
        NEW.item_id := NEW.entry_id;
    ELSIF TG_OP='UPDATE' AND (NEW.item_id IS DISTINCT FROM OLD.item_id
        OR NEW.entry_id IS DISTINCT FROM OLD.entry_id) THEN
        RAISE EXCEPTION 'wordlist item identity is immutable';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER wordlist_item_identity BEFORE INSERT OR UPDATE ON wordlist_items
    FOR EACH ROW EXECUTE FUNCTION wordlist_item_identity();
ALTER TABLE wordlist_review_requests ADD COLUMN item_snapshots jsonb;
ALTER TABLE wordlist_review_requests DROP CONSTRAINT wordlist_review_requests_entry_ids_check;
ALTER TABLE wordlist_review_requests ADD CONSTRAINT wordlist_review_requests_entry_ids_check
    CHECK (cardinality(entry_ids) BETWEEN 0 AND 10000);
ALTER TABLE wordlist_review_requests ADD CONSTRAINT wordlist_review_items_check
    CHECK ((cardinality(entry_ids)>0 OR (jsonb_typeof(item_snapshots)='array' AND jsonb_array_length(item_snapshots) BETWEEN 1 AND 10000)) IS TRUE);
