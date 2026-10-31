-- Derived evidence only: retain original dictionary contents and immutable questions.
SET LOCAL statement_timeout='5min';
CREATE FUNCTION dictionary.choice_translation_supported(item jsonb) RETURNS boolean LANGUAGE sql IMMUTABLE AS $$
 SELECT COALESCE(jsonb_typeof(item)='object'
 AND jsonb_typeof(COALESCE(item->'code',item->'lang_code'))='string'
 AND btrim(COALESCE(item->>'code',item->>'lang_code'))<>''
 AND (
  (jsonb_typeof(item->'word')='string' AND btrim(item->>'word')<>'')
  OR (
   NOT item ? 'word' AND jsonb_typeof(item->'note')='string' AND btrim(item->>'note')<>''
   AND COALESCE(item->>'code',item->>'lang_code') NOT IN ('zh','cmn','zho')
   AND NOT EXISTS(SELECT 1 FROM jsonb_object_keys(item) k WHERE k NOT IN ('code','lang_code','lang','note','tags','sense','_dis1'))
   AND (NOT item ? 'tags' OR (jsonb_typeof(item->'tags')='array' AND NOT EXISTS(SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(item->'tags')='array' THEN item->'tags' ELSE '[]' END) tag WHERE jsonb_typeof(tag)<>'string')))
  )
 ),false)
$$;
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
 WHERE NOT dictionary.choice_translation_supported(item));
$$;
DO $$ DECLARE r record; BEGIN
 FOR r IN SELECT dataset_id,source_key FROM dictionary.choice_semantics LOOP
  PERFORM dictionary.refresh_choice_semantics(r.dataset_id,r.source_key);
 END LOOP;
END $$;
ALTER TABLE learning_tasks ADD COLUMN choice_policy text CHECK (choice_policy IS NULL OR (choice_policy='strict_tiered_v1' AND question_type='choice_zh_to_en'));
