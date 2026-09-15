-- 恢复同一基本词性内英文缩写忽略大小写唯一。放开期间录入的重复缩写会让重建索引失败，
-- 先显式报出原因，需要手工改掉重复项再回退。
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM catalog.form_types
        GROUP BY part_of_speech_id, lower(abbreviation)
        HAVING count(*) > 1
    ) THEN
        RAISE EXCEPTION '同一基本词性下存在英文缩写重复的词形变化，请先改掉重复项再回退';
    END IF;
END
$$;

CREATE UNIQUE INDEX catalog_form_types_abbreviation_unique_idx
    ON catalog.form_types (part_of_speech_id, lower(abbreviation)) NULLS NOT DISTINCT;
