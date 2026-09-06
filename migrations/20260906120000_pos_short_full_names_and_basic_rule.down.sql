-- 只回退结构：被下线的非基础词性与细分词性种子不自动恢复，需从备份回灌。
DROP INDEX IF EXISTS catalog.catalog_parts_of_speech_full_name_en_unique_idx;
DROP INDEX IF EXISTS catalog.catalog_parts_of_speech_short_name_zh_unique_idx;

ALTER TABLE catalog.parts_of_speech
    DROP CONSTRAINT IF EXISTS catalog_parts_of_speech_full_name_en_check,
    DROP CONSTRAINT IF EXISTS catalog_parts_of_speech_short_name_zh_check,
    DROP COLUMN IF EXISTS full_name_en,
    DROP COLUMN IF EXISTS short_name_zh;

UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE;
