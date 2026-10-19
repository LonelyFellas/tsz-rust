-- Retire authoring grants without mapping them to any remaining permission.
WITH removed AS (
    DELETE FROM admin_permission_grants WHERE permission_key IN ('wordlists.create', 'wordlists.edit') RETURNING admin_id
)
UPDATE admins SET permission_version = permission_version + 1 WHERE id IN (SELECT admin_id FROM removed);

WITH removed AS (
    DELETE FROM permission_tag_items WHERE permission_key IN ('wordlists.create', 'wordlists.edit') RETURNING tag_id
)
UPDATE permission_tags SET version = version + 1, updated_at = now() WHERE id IN (SELECT tag_id FROM removed);
