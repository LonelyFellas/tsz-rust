-- 英文全称上限从 64 放宽到 200：基本词性、细分词性、词形变化三张表同口径。
-- 约束名保持不变：tests/catalog_schema.rs 按名字断言这三条约束。
ALTER TABLE catalog.parts_of_speech
    DROP CONSTRAINT catalog_parts_of_speech_full_name_en_check,
    ADD CONSTRAINT catalog_parts_of_speech_full_name_en_check
        CHECK (
            full_name_en = btrim(full_name_en)
            AND char_length(full_name_en) BETWEEN 1 AND 200
        );

ALTER TABLE catalog.sub_parts_of_speech
    DROP CONSTRAINT catalog_sub_parts_full_name_en_check,
    ADD CONSTRAINT catalog_sub_parts_full_name_en_check
        CHECK (
            full_name_en = btrim(full_name_en)
            AND char_length(full_name_en) BETWEEN 1 AND 200
        );

-- 词形变化表这条是建表时的列级约束，名字由 PostgreSQL 自动生成。
ALTER TABLE catalog.form_types
    DROP CONSTRAINT form_types_full_name_en_check,
    ADD CONSTRAINT form_types_full_name_en_check
        CHECK (
            full_name_en = btrim(full_name_en)
            AND char_length(full_name_en) BETWEEN 1 AND 200
        );
