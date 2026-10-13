-- No compatibility mapping: retired grants are removed, never restored or expanded.
-- Rollback also requires explicit reauthorization; historical audits remain unchanged.
WITH removed AS (
    DELETE FROM admin_permission_grants WHERE permission_key IN ('teacherapply.read_sensitive', 'lexicon_settings.edit') RETURNING admin_id
)
UPDATE admins SET permission_version = permission_version + 1 WHERE id IN (SELECT admin_id FROM removed);

WITH removed AS (
    DELETE FROM permission_tag_items WHERE permission_key IN ('teacherapply.read_sensitive', 'lexicon_settings.edit') RETURNING tag_id
)
UPDATE permission_tags SET version = version + 1, updated_at = now() WHERE id IN (SELECT tag_id FROM removed);
