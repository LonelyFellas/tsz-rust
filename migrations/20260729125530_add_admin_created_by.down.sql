DROP INDEX IF EXISTS admins_created_by_admin_id_idx;

ALTER TABLE admins
    DROP CONSTRAINT IF EXISTS admins_created_by_admin_id_fkey;

ALTER TABLE admins
    DROP COLUMN IF EXISTS created_by_admin_id;
