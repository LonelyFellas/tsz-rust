-- Retire recognition/validation grants without granting creation. Keep only the
-- removed rows and post-migration versions for a guarded, lossless rollback.
CREATE TABLE IF NOT EXISTS word_creation_retired_grants (
    admin_id uuid NOT NULL REFERENCES admins(id) ON DELETE CASCADE,
    permission_key text NOT NULL,
    granted_by uuid NOT NULL,
    granted_at timestamptz NOT NULL,
    expected_version bigint NOT NULL,
    PRIMARY KEY (admin_id, permission_key)
);
CREATE TABLE IF NOT EXISTS word_creation_retired_tag_items (
    tag_id uuid NOT NULL REFERENCES permission_tags(id) ON DELETE CASCADE,
    permission_key text NOT NULL,
    expected_version bigint NOT NULL,
    PRIMARY KEY (tag_id, permission_key)
);

SELECT id FROM admins WHERE id IN (
    SELECT admin_id FROM admin_permission_grants WHERE permission_key IN ('words.detect','words.validate')
) ORDER BY id FOR UPDATE;
SELECT id FROM permission_tags WHERE id IN (
    SELECT tag_id FROM permission_tag_items WHERE permission_key IN ('words.detect','words.validate')
) ORDER BY id FOR UPDATE;

INSERT INTO word_creation_retired_grants
SELECT g.admin_id,g.permission_key,g.granted_by,g.granted_at,a.permission_version+1
FROM admin_permission_grants g JOIN admins a ON a.id=g.admin_id
WHERE g.permission_key IN ('words.detect','words.validate')
ON CONFLICT DO NOTHING;
INSERT INTO word_creation_retired_tag_items
SELECT i.tag_id,i.permission_key,t.version+1
FROM permission_tag_items i JOIN permission_tags t ON t.id=i.tag_id
WHERE i.permission_key IN ('words.detect','words.validate')
ON CONFLICT DO NOTHING;

WITH removed AS (
    DELETE FROM admin_permission_grants WHERE permission_key IN ('words.detect','words.validate') RETURNING admin_id
)
UPDATE admins SET permission_version=permission_version+1 WHERE id IN (SELECT admin_id FROM removed);
WITH removed AS (
    DELETE FROM permission_tag_items WHERE permission_key IN ('words.detect','words.validate') RETURNING tag_id
)
UPDATE permission_tags SET version=version+1,updated_at=now() WHERE id IN (SELECT tag_id FROM removed);
