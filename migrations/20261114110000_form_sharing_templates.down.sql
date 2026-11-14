DROP VIEW catalog.form_sharing_template;
-- Drops template settings only; persisted entry content and sharing bindings are retained.
DROP TABLE catalog.form_sharing_members;
DROP TABLE catalog.form_sharing_groups;
DROP TABLE catalog.form_sharing_settings;
UPDATE catalog.metadata SET version=version+1,updated_at=now() WHERE id=TRUE;
