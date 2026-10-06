LOCK TABLE account_deletion_requests IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM account_deletion_requests) THEN
        RAISE EXCEPTION 'cannot remove account deletion consent or lifecycle evidence';
    END IF;
END $$;
DROP TABLE account_deletion_requests;
