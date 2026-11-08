LOCK TABLE users, admins IN ACCESS EXCLUSIVE MODE;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM users WHERE interface_locale IS NOT NULL)
       OR EXISTS (SELECT 1 FROM admins WHERE interface_locale IS NOT NULL) THEN
        RAISE EXCEPTION 'interface preferences exist; retain schema when rolling back application';
    END IF;
END $$;
ALTER TABLE admins DROP COLUMN interface_locale;
ALTER TABLE users DROP COLUMN interface_locale;
