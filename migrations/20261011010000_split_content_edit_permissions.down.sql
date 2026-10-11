-- Downgrading broadens content-only edit grants under the old backend.
-- Refuse an unsafe rollback after permissions have been narrowed.
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM admin_permission_grants g
        WHERE g.permission_key IN ('words.edit', 'sentences.edit')
          AND NOT EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id = g.admin_id AND a.permission_key = replace(g.permission_key, '.edit', '.associate'))
    ) OR EXISTS (
        SELECT 1 FROM permission_tag_items g
        WHERE g.permission_key IN ('words.edit', 'sentences.edit')
          AND NOT EXISTS (SELECT 1 FROM permission_tag_items a WHERE a.tag_id = g.tag_id AND a.permission_key = replace(g.permission_key, '.edit', '.associate'))
    ) THEN
        RAISE EXCEPTION 'Cannot downgrade split editing permissions: content-only grants require review';
    END IF;
END $$;
WITH removed AS (
    DELETE FROM admin_permission_grants WHERE permission_key IN ('words.associate', 'sentences.associate') RETURNING admin_id
)
UPDATE admins SET permission_version = permission_version + 1 WHERE id IN (SELECT admin_id FROM removed);
WITH removed AS (
    DELETE FROM permission_tag_items WHERE permission_key IN ('words.associate', 'sentences.associate') RETURNING tag_id
)
UPDATE permission_tags SET version = version + 1, updated_at = now() WHERE id IN (SELECT tag_id FROM removed);
