DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM admin_permission_grants)
        OR EXISTS (SELECT 1 FROM permission_tags)
        OR EXISTS (SELECT 1 FROM admins WHERE permission_version > 0)
        OR EXISTS (SELECT 1 FROM audit.admin_actions WHERE action LIKE 'admin.permissions.%' OR action LIKE 'admin.permission_tags.%') THEN
        RAISE EXCEPTION 'cannot remove permission schema containing grants or tags';
    END IF;
END $$;
DROP TABLE permission_tag_items;
DROP TABLE permission_tags;
DROP TABLE admin_permission_grants;
ALTER TABLE admins DROP COLUMN permission_version;
