ALTER TABLE catalog.form_types
    ADD COLUMN is_default boolean NOT NULL DEFAULT true,
    ADD CONSTRAINT catalog_form_types_base_default CHECK (code <> 'base' OR is_default);

UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE;
