-- Task #124: merge display restoration into publishing, including saved permission tags.
WITH added AS (
    INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by,granted_at)
    SELECT admin_id,'sentences.publish',granted_by,granted_at FROM admin_permission_grants
    WHERE permission_key='sentences.restore' ON CONFLICT DO NOTHING
), removed AS (
    DELETE FROM admin_permission_grants WHERE permission_key='sentences.restore' RETURNING admin_id
)
UPDATE admins SET permission_version=permission_version+1 WHERE id IN (SELECT admin_id FROM removed);
WITH added AS (
    INSERT INTO permission_tag_items(tag_id,permission_key)
    SELECT tag_id,'sentences.publish' FROM permission_tag_items
    WHERE permission_key='sentences.restore' ON CONFLICT DO NOTHING
), removed AS (
    DELETE FROM permission_tag_items WHERE permission_key='sentences.restore' RETURNING tag_id
)
UPDATE permission_tags SET version=version+1,updated_at=now() WHERE id IN (SELECT tag_id FROM removed);
