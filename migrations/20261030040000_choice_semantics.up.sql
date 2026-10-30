-- Derived evidence only; runtime never contacts the dictionary provider.
SET LOCAL statement_timeout='5min';
CREATE INDEX question_candidate_semantic_spelling ON lexicon.question_candidate_index((lexicon.question_candidate_key_v1(spelling)),publication_id);
CREATE FUNCTION dictionary.choice_words(value jsonb, field text) RETURNS text[] LANGUAGE sql IMMUTABLE AS $$
 SELECT COALESCE(array_agg(DISTINCT lexicon.question_candidate_key_v1(CASE WHEN jsonb_typeof(item)='string' THEN item#>>'{}' ELSE item->>'word' END)) FILTER(WHERE btrim(CASE WHEN jsonb_typeof(item)='string' THEN item#>>'{}' ELSE item->>'word' END)<>''),'{}')
 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(value->field)='array' THEN value->field ELSE '[]' END)item
$$;
CREATE FUNCTION dictionary.choice_strings(value jsonb, field text) RETURNS text[] LANGUAGE sql IMMUTABLE AS $$
 SELECT COALESCE(array_agg(DISTINCT lexicon.question_candidate_key_v1(item)) FILTER(WHERE btrim(item)<>''),'{}')
 FROM jsonb_array_elements_text(CASE WHEN jsonb_typeof(value->field)='array' THEN value->field ELSE '[]' END)item
$$;
CREATE FUNCTION dictionary.choice_supported(value jsonb) RETURNS boolean LANGUAGE sql IMMUTABLE AS $$
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
CREATE TABLE dictionary.choice_semantics (
 dataset_id bigint NOT NULL, source_key text NOT NULL, normalized_term text NOT NULL, pos text NOT NULL,
 domains text[] NOT NULL, equivalents text[] NOT NULL, antonyms text[] NOT NULL, meanings jsonb NOT NULL,
 PRIMARY KEY(dataset_id,source_key),
 FOREIGN KEY(dataset_id,source_key) REFERENCES dictionary.entry_contents(dataset_id,source_key) ON DELETE CASCADE
);
CREATE INDEX choice_semantics_term ON dictionary.choice_semantics(normalized_term,pos,dataset_id);
CREATE INDEX choice_semantics_domains ON dictionary.choice_semantics USING gin(domains);
CREATE INDEX choice_semantics_antonyms ON dictionary.choice_semantics USING gin(antonyms);
CREATE FUNCTION dictionary.refresh_choice_semantics(dataset bigint, source text) RETURNS void LANGUAGE sql AS $$
 DELETE FROM dictionary.choice_semantics WHERE dataset_id=dataset AND source_key=source;
 INSERT INTO dictionary.choice_semantics
 SELECT c.dataset_id,c.source_key,c.normalized_term,CASE c.pos WHEN 'adj' THEN 'adjective' WHEN 'adv' THEN 'adverb' WHEN 'prep' THEN 'preposition' WHEN 'pron' THEN 'pronoun' WHEN 'det' THEN 'determiner' WHEN 'conj' THEN 'conjunction' WHEN 'intj' THEN 'interjection' WHEN 'num' THEN 'numeral' ELSE c.pos END,
  ARRAY(SELECT DISTINCT domain FROM jsonb_array_elements(c.senses)s,
    LATERAL unnest(dictionary.choice_words(s,'hypernyms')||dictionary.choice_strings(s,'topics'))domain
    WHERE domain NOT IN ('entity','thing','object','person','individual','being','organism','act','action','event','state','quality','abstraction','relation','process','sciences','science','lifestyle','hobbies','general','countable','uncountable') AND length(domain)>1),
  ARRAY(SELECT DISTINCT word FROM jsonb_array_elements(c.senses)s,
    LATERAL unnest(dictionary.choice_words(s,'synonyms')||dictionary.choice_words(s,'alt_of')||dictionary.choice_words(s,'form_of'))word),
  ARRAY(SELECT DISTINCT word FROM jsonb_array_elements(c.senses)s,LATERAL unnest(dictionary.choice_words(s,'antonyms'))word),
  COALESCE((SELECT jsonb_agg(jsonb_build_object(
   'supported',dictionary.choice_supported(s),'id',s->>'id','tags',dictionary.choice_strings(s,'tags'),
   'glosses',dictionary.choice_strings(s,'glosses'),
   'equivalents',dictionary.choice_words(s,'synonyms')||dictionary.choice_words(s,'alt_of')||dictionary.choice_words(s,'form_of'),
   'parents',dictionary.choice_words(s,'hypernyms'),
   'children',dictionary.choice_words(s,'hyponyms'),
   'coordinates',dictionary.choice_words(s,'coordinate_terms'),
   'antonyms',dictionary.choice_words(s,'antonyms'),
   'translations',COALESCE((SELECT jsonb_agg(t->>'word') FROM jsonb_array_elements(CASE WHEN jsonb_typeof(s->'translations')='array' THEN s->'translations' ELSE '[]' END)t WHERE COALESCE(t->>'code',t->>'lang_code') IN ('zh','cmn','zho') AND jsonb_typeof(t->'word')='string'),'[]')
  )) FROM jsonb_array_elements(c.senses)s),'[]')
 FROM dictionary.entry_contents c WHERE c.dataset_id=dataset AND c.source_key=source
 AND jsonb_array_length(c.senses)<=256
 AND EXISTS(SELECT 1 FROM lexicon.question_candidate_index i JOIN lexicon.entries e ON e.id=i.entry_id AND e.current_publication_id=i.publication_id
 WHERE lexicon.question_candidate_key_v1(i.spelling)=c.normalized_term);
$$;
CREATE FUNCTION dictionary.refresh_choice_semantics_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 PERFORM dictionary.refresh_choice_semantics(NEW.dataset_id,NEW.source_key);RETURN NEW;
END $$;
CREATE TRIGGER choice_semantics_content AFTER INSERT OR UPDATE OF senses,normalized_term,pos ON dictionary.entry_contents
 FOR EACH ROW EXECUTE FUNCTION dictionary.refresh_choice_semantics_trigger();
-- Derive only spellings actually present in published learning candidates.
CREATE FUNCTION dictionary.refresh_choice_publication(publication uuid) RETURNS void LANGUAGE plpgsql AS $$
DECLARE r record;BEGIN
 FOR r IN SELECT DISTINCT c.dataset_id,c.source_key FROM lexicon.question_candidate_index i
 JOIN dictionary.entry_contents c ON c.normalized_term=lexicon.question_candidate_key_v1(i.spelling)
 WHERE i.publication_id=publication LOOP PERFORM dictionary.refresh_choice_semantics(r.dataset_id,r.source_key);END LOOP;
END $$;
CREATE FUNCTION dictionary.refresh_choice_publication_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_TABLE_SCHEMA='lexicon' AND TG_TABLE_NAME='entries' THEN
  PERFORM dictionary.refresh_choice_publication(NEW.current_publication_id);
 ELSE PERFORM dictionary.refresh_choice_publication(NEW.id);END IF;
 RETURN NEW;
END $$;
-- z sorts after the existing candidate-index refresh triggers.
CREATE TRIGGER z_choice_semantics_publication AFTER INSERT OR UPDATE OF snapshot,content_schema_version ON lexicon.entry_publications
 FOR EACH ROW EXECUTE FUNCTION dictionary.refresh_choice_publication_trigger();
CREATE TRIGGER z_choice_semantics_activation AFTER INSERT OR UPDATE OF current_publication_id ON lexicon.entries
 FOR EACH ROW EXECUTE FUNCTION dictionary.refresh_choice_publication_trigger();
DO $$ DECLARE r record; BEGIN
 FOR r IN SELECT DISTINCT e.current_publication_id FROM lexicon.entries e WHERE e.current_publication_id IS NOT NULL AND e.archived_at IS NULL LOOP
  PERFORM dictionary.refresh_choice_publication(r.current_publication_id);
 END LOOP;
END $$;
