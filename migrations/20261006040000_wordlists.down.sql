LOCK TABLE wordlists,wordlist_items,wordlist_review_requests IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM wordlists) OR EXISTS(SELECT 1 FROM wordlist_review_requests)
       OR EXISTS(SELECT 1 FROM audit.admin_actions WHERE resource_type='wordlist') THEN
        RAISE EXCEPTION 'cannot remove wordlist content or review evidence';
    END IF;
END $$;
DROP TABLE wordlist_review_requests,wordlist_items,wordlists;
DROP FUNCTION wordlists_preserve_owner();
