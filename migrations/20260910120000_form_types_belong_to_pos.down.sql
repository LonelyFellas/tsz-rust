-- 回退到全局词形目录。若不同基本词性下已存在重名词形，恢复全局唯一索引会失败，
-- 需要先手工消歧再回退。
DROP INDEX catalog.catalog_form_types_pos_order_idx;
DROP INDEX catalog.catalog_form_types_name_zh_unique_idx;
DROP INDEX catalog.catalog_form_types_name_en_unique_idx;
DROP INDEX catalog.catalog_form_types_short_name_zh_unique_idx;
DROP INDEX catalog.catalog_form_types_abbreviation_unique_idx;
DROP INDEX catalog.catalog_form_types_full_name_en_unique_idx;
CREATE UNIQUE INDEX catalog_form_types_name_zh_unique_idx ON catalog.form_types (name_zh);
CREATE UNIQUE INDEX catalog_form_types_name_en_unique_idx ON catalog.form_types (lower(name_en));
CREATE UNIQUE INDEX catalog_form_types_short_name_zh_unique_idx ON catalog.form_types (short_name_zh);
CREATE UNIQUE INDEX catalog_form_types_abbreviation_unique_idx ON catalog.form_types (lower(abbreviation));
CREATE UNIQUE INDEX catalog_form_types_full_name_en_unique_idx ON catalog.form_types (lower(full_name_en));
ALTER TABLE catalog.form_types DROP CONSTRAINT catalog_form_types_base_is_global;
ALTER TABLE catalog.form_types DROP COLUMN part_of_speech_id;
