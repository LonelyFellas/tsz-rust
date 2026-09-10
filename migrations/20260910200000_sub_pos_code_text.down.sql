-- 回退会重新要求 name_en 在同一基本词性下唯一。上线后管理员很可能已经按新口径填了重复的
-- 正式英文（这正是本次改动的目的），那时这条回退不该以裸 23505 收场：deployment_migrations
-- 的 undo 是单事务原子的，一条看不懂的唯一冲突会把整串回退一起打回。先守卫、给出可读原因。
DO $$
DECLARE
    duplicates TEXT;
BEGIN
    SELECT string_agg(name_en, ', ')
    INTO duplicates
    FROM (
        SELECT lower(name_en) AS name_en
        FROM catalog.sub_parts_of_speech
        GROUP BY part_of_speech_id, lower(name_en)
        HAVING count(*) > 1
    ) duplicated;

    IF duplicates IS NOT NULL THEN
        RAISE EXCEPTION
            'cannot restore catalog_sub_parts_name_en_unique_idx while duplicate name_en exist under the same part of speech: %',
            duplicates;
    END IF;
END
$$;

CREATE UNIQUE INDEX catalog_sub_parts_name_en_unique_idx
    ON catalog.sub_parts_of_speech (
        part_of_speech_id,
        lower(name_en)
    );
