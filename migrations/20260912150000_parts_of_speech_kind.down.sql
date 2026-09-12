-- 回退：删 kind 列会把短语词性静默变成单词词性，重建全局唯一索引也可能撞车，
-- 所以只要还有短语词性就拒绝回退，报出 code 让运维先处理。
DO $$
DECLARE
    phrase_codes TEXT;
BEGIN
    SELECT string_agg(code, ', ' ORDER BY code)
    INTO phrase_codes
    FROM catalog.parts_of_speech
    WHERE kind = 'phrase';
    IF phrase_codes IS NOT NULL THEN
        RAISE EXCEPTION
            'cannot drop parts_of_speech.kind while phrase parts of speech exist: %',
            phrase_codes;
    END IF;
END
$$;

ALTER TABLE catalog.form_types
    DROP CONSTRAINT catalog_form_types_part_of_speech_fkey,
    ADD CONSTRAINT catalog_form_types_part_of_speech_fkey
        FOREIGN KEY (part_of_speech_id)
        REFERENCES catalog.parts_of_speech(id)
        ON DELETE RESTRICT,
    DROP COLUMN part_of_speech_kind;

ALTER TABLE lexicon.entry_pos
    DROP CONSTRAINT lexicon_entry_pos_catalog_pos_fkey,
    ADD CONSTRAINT lexicon_entry_pos_catalog_pos_fkey
        FOREIGN KEY (part_of_speech_id)
        REFERENCES catalog.parts_of_speech(id)
        ON DELETE RESTRICT,
    DROP CONSTRAINT lexicon_entry_pos_entry_kind_fkey,
    DROP CONSTRAINT lexicon_entry_pos_entry_kind_check,
    DROP COLUMN entry_kind;
ALTER TABLE lexicon.entries
    DROP CONSTRAINT lexicon_entries_id_kind_key;

DROP INDEX catalog.catalog_parts_of_speech_name_zh_unique_idx;
DROP INDEX catalog.catalog_parts_of_speech_name_en_unique_idx;
DROP INDEX catalog.catalog_parts_of_speech_abbreviation_unique_idx;
DROP INDEX catalog.catalog_parts_of_speech_short_name_zh_unique_idx;
DROP INDEX catalog.catalog_parts_of_speech_full_name_en_unique_idx;
CREATE UNIQUE INDEX catalog_parts_of_speech_name_zh_unique_idx
    ON catalog.parts_of_speech (name_zh);
CREATE UNIQUE INDEX catalog_parts_of_speech_name_en_unique_idx
    ON catalog.parts_of_speech (lower(name_en));
CREATE UNIQUE INDEX catalog_parts_of_speech_abbreviation_unique_idx
    ON catalog.parts_of_speech (lower(abbreviation));
CREATE UNIQUE INDEX catalog_parts_of_speech_short_name_zh_unique_idx
    ON catalog.parts_of_speech (short_name_zh);
CREATE UNIQUE INDEX catalog_parts_of_speech_full_name_en_unique_idx
    ON catalog.parts_of_speech (lower(full_name_en));

ALTER TABLE catalog.parts_of_speech
    DROP CONSTRAINT catalog_parts_of_speech_id_kind_key,
    DROP CONSTRAINT catalog_parts_of_speech_phrase_code_check,
    DROP CONSTRAINT catalog_parts_of_speech_kind_check,
    DROP COLUMN kind;

UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE;
