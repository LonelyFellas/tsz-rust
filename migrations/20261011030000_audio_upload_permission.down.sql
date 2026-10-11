-- Old code grants uploads with content-write permission. Refuse to widen narrowed grants.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM admin_permission_grants g WHERE ((g.permission_key IN ('words.create','words.edit') AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='words.access')) OR (g.permission_key='sentences.create' AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='sentences.access')) OR (g.permission_key='sentences.edit' AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='sentences.access') AND EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='words.access')))
        AND NOT EXISTS (SELECT 1 FROM admin_permission_grants a WHERE a.admin_id=g.admin_id AND a.permission_key='speech.upload_audio'))
    OR EXISTS (SELECT 1 FROM permission_tag_items g WHERE g.permission_key IN ('words.create','words.edit','sentences.create','sentences.edit')
        AND NOT EXISTS (SELECT 1 FROM permission_tag_items a WHERE a.tag_id=g.tag_id AND a.permission_key='speech.upload_audio')) THEN
        RAISE EXCEPTION 'Cannot downgrade audio upload permissions: content-only grants require review';
    END IF;
END $$;
WITH removed AS (DELETE FROM admin_permission_grants WHERE permission_key='speech.upload_audio' RETURNING admin_id)
UPDATE admins SET permission_version=permission_version+1 WHERE id IN (SELECT admin_id FROM removed);
WITH removed AS (DELETE FROM permission_tag_items WHERE permission_key='speech.upload_audio' RETURNING tag_id)
UPDATE permission_tags SET version=version+1,updated_at=now() WHERE id IN (SELECT tag_id FROM removed);
