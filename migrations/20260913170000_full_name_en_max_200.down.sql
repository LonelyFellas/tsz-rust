DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM catalog.parts_of_speech WHERE char_length(full_name_en) > 64)
        OR EXISTS (SELECT 1 FROM catalog.sub_parts_of_speech WHERE char_length(full_name_en) > 64)
        OR EXISTS (SELECT 1 FROM catalog.form_types WHERE char_length(full_name_en) > 64)
    THEN
        RAISE EXCEPTION '存在超过 64 字的英文全称，回退前请先把它们改短';
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
