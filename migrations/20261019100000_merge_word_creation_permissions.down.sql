SELECT id FROM admins WHERE id IN (SELECT admin_id FROM word_creation_retired_grants) ORDER BY id FOR UPDATE;
SELECT id FROM permission_tags WHERE id IN (SELECT tag_id FROM word_creation_retired_tag_items) ORDER BY id FOR UPDATE;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM word_creation_retired_grants r JOIN admins a ON a.id=r.admin_id
        WHERE a.permission_version<>r.expected_version
           OR NOT EXISTS (SELECT 1 FROM admins actor WHERE actor.id=r.granted_by)
           OR EXISTS (SELECT 1 FROM admin_permission_grants g WHERE g.admin_id=r.admin_id AND g.permission_key=r.permission_key)
    ) OR EXISTS (
        SELECT 1 FROM word_creation_retired_tag_items r JOIN permission_tags t ON t.id=r.tag_id
        WHERE t.version<>r.expected_version
           OR EXISTS (SELECT 1 FROM permission_tag_items i WHERE i.tag_id=r.tag_id AND i.permission_key=r.permission_key)
    ) OR EXISTS (SELECT 1 FROM admin_permission_grants WHERE permission_key='speech.global_dubbing')
      OR EXISTS (SELECT 1 FROM permission_tag_items WHERE permission_key='speech.global_dubbing') THEN
        RAISE EXCEPTION 'word creation permission rollback conflicts with later authorization; review and explicitly reauthorize before rollback';
    END IF;
END $$;

INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by,granted_at)
SELECT admin_id,permission_key,granted_by,granted_at FROM word_creation_retired_grants;
INSERT INTO permission_tag_items(tag_id,permission_key)
SELECT tag_id,permission_key FROM word_creation_retired_tag_items;
UPDATE admins SET permission_version=permission_version+1 WHERE id IN (SELECT admin_id FROM word_creation_retired_grants);
UPDATE permission_tags SET version=version+1,updated_at=now() WHERE id IN (SELECT tag_id FROM word_creation_retired_tag_items);
DROP TABLE word_creation_retired_tag_items;
DROP TABLE word_creation_retired_grants;
