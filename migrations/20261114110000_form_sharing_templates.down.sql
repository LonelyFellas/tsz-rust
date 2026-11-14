-- Even an empty saved template is configuration; never discard it during recovery.
LOCK TABLE catalog.form_sharing_settings, catalog.form_sharing_groups,
    catalog.form_sharing_members IN ACCESS EXCLUSIVE MODE;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM catalog.form_sharing_settings)
        OR EXISTS (SELECT 1 FROM catalog.form_sharing_groups)
        OR EXISTS (SELECT 1 FROM catalog.form_sharing_members) THEN
        RAISE EXCEPTION 'default form sharing templates require manual recovery while configuration exists';
    END IF;
END
$$;

DROP VIEW catalog.form_sharing_template;
-- Drops template settings only; persisted entry content and sharing bindings are retained.
DROP TABLE catalog.form_sharing_members;
DROP TABLE catalog.form_sharing_groups;
DROP TABLE catalog.form_sharing_settings;
UPDATE catalog.metadata SET version=version+1,updated_at=now() WHERE id=TRUE;
