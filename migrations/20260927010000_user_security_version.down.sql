DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM users WHERE security_version > 0) THEN
        RAISE EXCEPTION 'cannot remove security_version after credentials have changed';
    END IF;
END $$;
ALTER TABLE users DROP COLUMN security_version;
