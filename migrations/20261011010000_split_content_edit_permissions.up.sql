-- Preserve the capabilities of existing editors while making future grants independent.
WITH added AS (
    INSERT INTO admin_permission_grants (admin_id, permission_key, granted_by, granted_at)
    SELECT g.admin_id, replace(g.permission_key, '.edit', '.associate'), g.granted_by, g.granted_at
    FROM admin_permission_grants g
    WHERE g.permission_key IN ('words.edit', 'sentences.edit')
      AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id = g.admin_id AND a.permission_key = split_part(g.permission_key, '.', 1) || '.access')
      AND (g.permission_key = 'words.edit' OR EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id = g.admin_id AND a.permission_key = 'words.access'))
    ON CONFLICT DO NOTHING RETURNING admin_id
)
UPDATE admins SET permission_version = permission_version + 1 WHERE id IN (SELECT admin_id FROM added);
WITH added AS (
    INSERT INTO permission_tag_items (tag_id, permission_key)
    SELECT tag_id, replace(permission_key, '.edit', '.associate')
    FROM permission_tag_items WHERE permission_key IN ('words.edit', 'sentences.edit')
    ON CONFLICT DO NOTHING RETURNING tag_id
)
UPDATE permission_tags SET version = version + 1, updated_at = now() WHERE id IN (SELECT tag_id FROM added);
