DROP INDEX IF EXISTS catalog.catalog_sub_parts_full_name_en_unique_idx;

ALTER TABLE catalog.sub_parts_of_speech
    DROP CONSTRAINT IF EXISTS catalog_sub_parts_full_name_en_check,
    DROP CONSTRAINT IF EXISTS catalog_sub_parts_abbreviation_check,
    DROP CONSTRAINT IF EXISTS catalog_sub_parts_short_name_zh_check,
    DROP COLUMN IF EXISTS full_name_en,
    DROP COLUMN IF EXISTS abbreviation,
    DROP COLUMN IF EXISTS short_name_zh;

UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE;
