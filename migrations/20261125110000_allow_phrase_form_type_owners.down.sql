DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM catalog.form_types f
        JOIN catalog.parts_of_speech p ON p.id = f.part_of_speech_id
        WHERE p.kind = 'phrase'
    ) THEN
        RAISE EXCEPTION 'cannot roll back while phrase form types exist';
    END IF;
END $$;

ALTER TABLE catalog.form_types
    DROP CONSTRAINT catalog_form_types_part_of_speech_fkey,
    ADD COLUMN part_of_speech_kind TEXT NOT NULL DEFAULT 'word'
        CONSTRAINT catalog_form_types_part_of_speech_kind_check CHECK (part_of_speech_kind = 'word'),
    ADD CONSTRAINT catalog_form_types_part_of_speech_fkey
        FOREIGN KEY (part_of_speech_id, part_of_speech_kind)
        REFERENCES catalog.parts_of_speech(id, kind) ON DELETE RESTRICT;
