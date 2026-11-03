-- Dropping persisted default-template settings and changing the catalog epoch is not a
-- lossless deployment rollback. Keep the database intact for an approved manual plan.
DO $$ BEGIN
    RAISE EXCEPTION 'default form templates require manual recovery; automatic downgrade is not supported';
END $$;
