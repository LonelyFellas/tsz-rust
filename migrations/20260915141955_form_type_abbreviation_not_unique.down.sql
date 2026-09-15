-- 恢复同一基本词性内英文缩写忽略大小写唯一。放开期间录入的重复缩写会让重建索引撞裸 23505，
-- 而 deployment_migrations 的 undo 是单事务原子的，会把整串回退一起打回：先守卫，报出是哪几条。
DO $$
DECLARE
    duplicates TEXT;
BEGIN
    SELECT string_agg(p.code || ' 下的 ' || d.form_codes, '；' ORDER BY p.code, d.form_codes)
    INTO duplicates
    FROM (
        SELECT part_of_speech_id, string_agg(code, '/' ORDER BY code) AS form_codes
        FROM catalog.form_types
        GROUP BY part_of_speech_id, lower(abbreviation)
        HAVING count(*) > 1
    ) d
    JOIN catalog.parts_of_speech p ON p.id = d.part_of_speech_id;

    IF duplicates IS NOT NULL THEN
        RAISE EXCEPTION '同一基本词性下存在英文缩写重复的词形变化，请先改掉重复项再回退：%', duplicates;
    END IF;
END
$$;

CREATE UNIQUE INDEX catalog_form_types_abbreviation_unique_idx
    ON catalog.form_types (part_of_speech_id, lower(abbreviation)) NULLS NOT DISTINCT;
