ALTER TABLE catalog.form_types
    DROP CONSTRAINT catalog_form_types_part_of_speech_fkey,
    DROP COLUMN part_of_speech_kind,
    ADD CONSTRAINT catalog_form_types_part_of_speech_fkey
        FOREIGN KEY (part_of_speech_id) REFERENCES catalog.parts_of_speech(id) ON DELETE RESTRICT;
