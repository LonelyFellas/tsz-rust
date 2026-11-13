-- 回退会永久丢失这两类目录配置；先导出需要保留的数据。
DROP TABLE catalog.lexicon_taxonomies;
WITH removed AS (
    DELETE FROM admin_permission_grants WHERE permission_key = 'lexicon_settings.edit_taxonomies' RETURNING admin_id
)
UPDATE admins SET permission_version = permission_version + 1 WHERE id IN (SELECT admin_id FROM removed);
WITH removed AS (
    DELETE FROM permission_tag_items WHERE permission_key = 'lexicon_settings.edit_taxonomies' RETURNING tag_id
)
UPDATE permission_tags SET version = version + 1, updated_at = now() WHERE id IN (SELECT tag_id FROM removed);
