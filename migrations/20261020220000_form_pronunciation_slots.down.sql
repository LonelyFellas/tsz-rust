DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.v3_form_pronunciation_mappings) THEN
        RAISE EXCEPTION 'pronunciation identity mappings are present; retain the compatible backend instead of a lossy rollback';
    END IF;
END
$$;
DROP TABLE lexicon.v3_form_pronunciation_mapping_slots;
DROP TABLE lexicon.v3_form_pronunciation_mappings;
