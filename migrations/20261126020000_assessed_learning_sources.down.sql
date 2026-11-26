DO $$
BEGIN
    IF EXISTS (SELECT FROM learning_source.reviews)
       OR EXISTS (SELECT FROM admin_permission_grants WHERE permission_key='words.review_assessment')
       OR EXISTS (SELECT FROM permission_tag_items WHERE permission_key='words.review_assessment')
       OR EXISTS (
        SELECT FROM wordlist_items i JOIN learning_source.identities s ON s.next_id=i.entry_id
    ) THEN RAISE EXCEPTION 'Assessment history, permissions or student references exist; rollback would lose data';
    END IF;
END $$;
ALTER TABLE wordlist_items DROP CONSTRAINT wordlist_items_source_fkey,
    ADD CONSTRAINT wordlist_items_entry_id_fkey FOREIGN KEY(entry_id) REFERENCES lexicon.entries ON DELETE RESTRICT;
DROP TRIGGER register_learning_identity ON lexicon.entries;
CREATE OR REPLACE FUNCTION dictionary.refresh_choice_semantics(dataset bigint, source text) RETURNS void LANGUAGE sql AS $$
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

DROP VIEW learning_source.search_surfaces, learning_source.current_publications, learning_source.choice_candidates, learning_source.publications, learning_source.entries, learning_source.next_entries;
DROP TABLE learning_source.next_choice_candidates, learning_source.next_surfaces, learning_source.reviews, learning_source.reviewed_publications, learning_source.identities;
DROP FUNCTION learning_source.register_legacy();
DROP SCHEMA learning_source;
