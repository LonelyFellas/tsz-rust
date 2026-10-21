-- Never turn shared data into independent copies or rewrite immutable history.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.v3_form_content_bindings GROUP BY content_id HAVING count(*) > 1)
       OR EXISTS (SELECT 1 FROM lexicon.entry_publications WHERE snapshot->'forms'->>'forms_format' = 'shared_v1')
       OR EXISTS (SELECT 1 FROM lexicon.entry_editor_projection WHERE forms->>'forms_format' = 'shared_v1') THEN
        RAISE EXCEPTION 'shared form content is present; retain the compatible backend instead of a lossy rollback';
    END IF;
END
$$;
DROP FUNCTION lexicon.v3_legacy_forms_to_shared(JSONB);
DROP FUNCTION lexicon.v3_legacy_content_slot(TEXT, UUID);
DROP TABLE lexicon.v3_form_content_node_slots;
DROP TABLE lexicon.v3_form_content_slots;
DROP TABLE lexicon.v3_form_content_bindings;
DROP TABLE lexicon.v3_form_contents;
