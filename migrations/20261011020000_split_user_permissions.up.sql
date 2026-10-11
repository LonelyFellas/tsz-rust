-- Preserve effective legacy access without granting access to additional modules.

WITH added AS (INSERT INTO admin_permission_grants (admin_id, permission_key, granted_by, granted_at)
SELECT g.admin_id, 'users.read_basic', g.granted_by, g.granted_at FROM admin_permission_grants g WHERE g.permission_key='users.access'
UNION ALL
SELECT g.admin_id, 'users.read_coin_balance', g.granted_by, g.granted_at FROM admin_permission_grants g WHERE g.permission_key='users.access' AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='coins.access')
UNION ALL
SELECT g.admin_id, 'users.read_coin_entries', g.granted_by, g.granted_at FROM admin_permission_grants g WHERE g.permission_key='users.access' AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='coins.access')
UNION ALL
SELECT g.admin_id, 'users.read_identity', g.granted_by, g.granted_at FROM admin_permission_grants g WHERE g.permission_key='users.access' AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='teacherapply.access') AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='teacherapply.read_sensitive')
ON CONFLICT DO NOTHING RETURNING admin_id)
UPDATE admins SET permission_version = permission_version + 1 WHERE id IN (SELECT admin_id FROM added);

WITH added AS (INSERT INTO permission_tag_items (tag_id, permission_key)
SELECT g.tag_id, 'users.read_basic' FROM permission_tag_items g WHERE g.permission_key='users.access'
UNION ALL
SELECT g.tag_id, 'users.read_coin_balance' FROM permission_tag_items g WHERE g.permission_key='users.access' AND EXISTS (SELECT 1 FROM permission_tag_items a WHERE a.tag_id=g.tag_id AND a.permission_key='coins.access')
UNION ALL
SELECT g.tag_id, 'users.read_coin_entries' FROM permission_tag_items g WHERE g.permission_key='users.access' AND EXISTS (SELECT 1 FROM permission_tag_items a WHERE a.tag_id=g.tag_id AND a.permission_key='coins.access')
UNION ALL
SELECT g.tag_id, 'users.read_identity' FROM permission_tag_items g WHERE g.permission_key='users.access' AND EXISTS (SELECT 1 FROM permission_tag_items a WHERE a.tag_id=g.tag_id AND a.permission_key='teacherapply.access') AND EXISTS (SELECT 1 FROM permission_tag_items a WHERE a.tag_id=g.tag_id AND a.permission_key='teacherapply.read_sensitive')
ON CONFLICT DO NOTHING RETURNING tag_id)
UPDATE permission_tags SET version = version + 1, updated_at = now() WHERE id IN (SELECT tag_id FROM added);
