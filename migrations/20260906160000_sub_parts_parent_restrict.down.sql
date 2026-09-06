ALTER TABLE catalog.sub_parts_of_speech
    DROP CONSTRAINT catalog_sub_parts_parent_fkey,
    ADD CONSTRAINT catalog_sub_parts_parent_fkey
        FOREIGN KEY (part_of_speech_id)
        REFERENCES catalog.parts_of_speech(id)
        ON DELETE CASCADE;
