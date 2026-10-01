DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM admins WHERE security_version > 0) THEN
        RAISE EXCEPTION 'cannot remove admin security_version after credentials changed';
    END IF;
END $$;
ALTER TABLE admins DROP COLUMN security_version;
