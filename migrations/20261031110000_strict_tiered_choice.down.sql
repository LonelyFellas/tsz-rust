DO $$ BEGIN IF EXISTS(SELECT 1 FROM learning_tasks WHERE choice_policy IS NOT NULL) THEN RAISE EXCEPTION 'strict choice tasks exist; rollback requires a service that supports their policy'; END IF; END $$;
ALTER TABLE learning_tasks DROP COLUMN choice_policy;
CREATE OR REPLACE FUNCTION dictionary.choice_supported(value jsonb) RETURNS boolean LANGUAGE sql IMMUTABLE AS $$
 SELECT jsonb_typeof(value)='object'
 AND NOT EXISTS(SELECT 1 FROM unnest(ARRAY['glosses','tags','synonyms','alt_of','form_of','hypernyms','hyponyms','coordinate_terms','antonyms','translations'])field
 WHERE value ? field AND jsonb_typeof(value->field)<>'array')
 AND NOT EXISTS(SELECT 1 FROM unnest(ARRAY['synonyms','alt_of','form_of','hypernyms','hyponyms','coordinate_terms','antonyms'])field,
 LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(value->field)='array' THEN value->field ELSE '[]' END)item
 WHERE NOT COALESCE(((jsonb_typeof(item)='string' AND btrim(item#>>'{}')<>'') OR (jsonb_typeof(item)='object' AND jsonb_typeof(item->'word')='string' AND btrim(item->>'word')<>'')),false))
 AND NOT EXISTS(SELECT 1 FROM unnest(ARRAY['glosses','tags'])field,
 LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(value->field)='array' THEN value->field ELSE '[]' END)item WHERE jsonb_typeof(item)<>'string')
 AND NOT EXISTS(SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(value->'translations')='array' THEN value->'translations' ELSE '[]' END)item
 WHERE NOT COALESCE(jsonb_typeof(item)='object' AND jsonb_typeof(item->'word')='string' AND btrim(item->>'word')<>'' AND jsonb_typeof(COALESCE(item->'code',item->'lang_code'))='string',false));
$$;
DROP FUNCTION dictionary.choice_translation_supported(jsonb);
DO $$ DECLARE r record; BEGIN
 FOR r IN SELECT dataset_id,source_key FROM dictionary.choice_semantics LOOP
  PERFORM dictionary.refresh_choice_semantics(r.dataset_id,r.source_key);
 END LOOP;
END $$;
