-- Preserve only currently effective upload capabilities; do not add access to another module.
WITH added AS (
    INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by,granted_at)
    SELECT DISTINCT ON (g.admin_id) g.admin_id,'speech.upload_audio',g.granted_by,g.granted_at
    FROM admin_permission_grants g WHERE ((g.permission_key IN ('words.create','words.edit') AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='words.access')) OR (g.permission_key='sentences.create' AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='sentences.access')) OR (g.permission_key='sentences.edit' AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='sentences.access') AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='words.access')))
    ORDER BY g.admin_id,g.granted_at,g.permission_key
    ON CONFLICT DO NOTHING RETURNING admin_id
)
UPDATE admins SET permission_version=permission_version+1 WHERE id IN (SELECT admin_id FROM added);
WITH added AS (
    INSERT INTO permission_tag_items(tag_id,permission_key)
    SELECT DISTINCT tag_id,'speech.upload_audio' FROM permission_tag_items
    WHERE permission_key IN ('words.create','words.edit','sentences.create','sentences.edit')
    ON CONFLICT DO NOTHING RETURNING tag_id
)
UPDATE permission_tags SET version=version+1,updated_at=now() WHERE id IN (SELECT tag_id FROM added);
