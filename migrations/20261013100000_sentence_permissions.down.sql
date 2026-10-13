-- The merged grant cannot reconstruct the former publish/restore distinction.
-- Preserve the merged capabilities for publishers on downgrade; published-only readers lose access.
WITH added AS (
    INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by,granted_at)
    SELECT admin_id,'sentences.restore',granted_by,granted_at FROM admin_permission_grants
    WHERE permission_key='sentences.publish' ON CONFLICT DO NOTHING RETURNING admin_id
), removed AS (
    DELETE FROM admin_permission_grants WHERE permission_key='sentences.read_published' RETURNING admin_id
)
UPDATE admins SET permission_version=permission_version+1 WHERE id IN (SELECT admin_id FROM added UNION SELECT admin_id FROM removed);
WITH added AS (
    INSERT INTO permission_tag_items(tag_id,permission_key)
    SELECT tag_id,'sentences.restore' FROM permission_tag_items
    WHERE permission_key='sentences.publish' ON CONFLICT DO NOTHING RETURNING tag_id
), removed AS (
    DELETE FROM permission_tag_items WHERE permission_key='sentences.read_published' RETURNING tag_id
)
UPDATE permission_tags SET version=version+1,updated_at=now() WHERE id IN (SELECT tag_id FROM added UNION SELECT tag_id FROM removed);
