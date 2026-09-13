-- 回退：上限收回 64 会让超长的存量英文全称撞 CHECK，所以只要还有超过 64 字的就拒绝回退，
-- 报出表名与 code 让运维先改短。
DO $$
DECLARE
    long_names TEXT;
BEGIN
    SELECT string_agg(item, ', ' ORDER BY item)
    INTO long_names
    FROM (
        SELECT 'parts_of_speech:' || code AS item
        FROM catalog.parts_of_speech
        WHERE char_length(full_name_en) > 64
        UNION ALL
        SELECT 'sub_parts_of_speech:' || code
        FROM catalog.sub_parts_of_speech
        WHERE char_length(full_name_en) > 64
        UNION ALL
        SELECT 'form_types:' || code
        FROM catalog.form_types
        WHERE char_length(full_name_en) > 64
    ) long_rows;
    IF long_names IS NOT NULL THEN
        RAISE EXCEPTION
            'cannot restore full_name_en max length 64 while longer values exist: %',
            long_names;
    END IF;
END
$$;

ALTER TABLE catalog.parts_of_speech
    DROP CONSTRAINT catalog_parts_of_speech_full_name_en_check,
    ADD CONSTRAINT catalog_parts_of_speech_full_name_en_check
        CHECK (
            full_name_en = btrim(full_name_en)
            AND char_length(full_name_en) BETWEEN 1 AND 64
        );

ALTER TABLE catalog.sub_parts_of_speech
    DROP CONSTRAINT catalog_sub_parts_full_name_en_check,
    ADD CONSTRAINT catalog_sub_parts_full_name_en_check
        CHECK (
            full_name_en = btrim(full_name_en)
            AND char_length(full_name_en) BETWEEN 1 AND 64
        );

ALTER TABLE catalog.form_types
    DROP CONSTRAINT form_types_full_name_en_check,
    ADD CONSTRAINT form_types_full_name_en_check
        CHECK (
            full_name_en = btrim(full_name_en)
            AND char_length(full_name_en) BETWEEN 1 AND 64
        );
