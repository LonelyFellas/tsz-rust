-- 回退会丢失默认模版开关设置，所有可用词形恢复为默认模版。
ALTER TABLE catalog.form_types
    DROP CONSTRAINT catalog_form_types_base_default,
    DROP COLUMN is_default;

UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE;
