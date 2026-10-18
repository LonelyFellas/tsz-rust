LOCK TABLE wordlist_items,wordlist_review_requests IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM wordlist_items WHERE entry_id IS NULL)
       OR EXISTS(SELECT 1 FROM wordlist_review_requests r, jsonb_array_elements(r.item_snapshots) i WHERE i->'custom_word' IS NOT NULL AND i->'custom_word'<>'null'::jsonb) THEN
        RAISE EXCEPTION 'custom wordlist content prevents destructive rollback';
    END IF;
END $$;
ALTER TABLE wordlist_review_requests DROP CONSTRAINT wordlist_review_items_check;
ALTER TABLE wordlist_review_requests DROP CONSTRAINT wordlist_review_requests_entry_ids_check;
ALTER TABLE wordlist_review_requests ADD CHECK (cardinality(entry_ids) BETWEEN 1 AND 10000);
ALTER TABLE wordlist_review_requests DROP COLUMN item_snapshots;
DROP TRIGGER wordlist_item_identity ON wordlist_items;
DROP FUNCTION wordlist_item_identity();
ALTER TABLE wordlist_items DROP CONSTRAINT wordlist_item_source;
ALTER TABLE wordlist_items DROP COLUMN custom_word;
ALTER TABLE wordlist_items DROP CONSTRAINT wordlist_items_pkey;
ALTER TABLE wordlist_items DROP CONSTRAINT wordlist_items_wordlist_id_entry_id_key;
ALTER TABLE wordlist_items ALTER COLUMN entry_id SET NOT NULL;
ALTER TABLE wordlist_items ADD PRIMARY KEY(wordlist_id,entry_id);
ALTER TABLE wordlist_items DROP COLUMN item_id;
