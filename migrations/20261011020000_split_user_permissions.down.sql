-- Old backends interpret users.access as full basic-profile access.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM admin_permission_grants g WHERE g.permission_key='users.access' AND NOT EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='users.read_basic')) OR EXISTS (SELECT 1 FROM permission_tag_items g WHERE g.permission_key='users.access' AND NOT EXISTS (SELECT 1 FROM permission_tag_items a WHERE a.tag_id=g.tag_id AND a.permission_key='users.read_basic')) THEN
        RAISE EXCEPTION 'Cannot downgrade user permissions: list-only grants require review';
    END IF;
END $$;

WITH removed AS (DELETE FROM admin_permission_grants WHERE permission_key IN ('users.read_basic', 'users.read_coin_balance', 'users.read_coin_entries', 'users.read_identity') RETURNING admin_id)
UPDATE admins SET permission_version = permission_version + 1 WHERE id IN (SELECT admin_id FROM removed);

WITH removed AS (DELETE FROM permission_tag_items WHERE permission_key IN ('users.read_basic', 'users.read_coin_balance', 'users.read_coin_entries', 'users.read_identity') RETURNING tag_id)
UPDATE permission_tags SET version = version + 1, updated_at = now() WHERE id IN (SELECT tag_id FROM removed);
