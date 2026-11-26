CREATE SCHEMA learning_source;

-- Keep relational deletion protection for both origins, including existing lists.
CREATE TABLE learning_source.identities (
    id uuid PRIMARY KEY,
    legacy_id uuid UNIQUE REFERENCES lexicon.entries ON DELETE CASCADE,
    next_id uuid UNIQUE REFERENCES lexicon_next.entries ON DELETE CASCADE,
    CHECK (num_nonnulls(legacy_id,next_id)=1 AND id=coalesce(legacy_id,next_id))
);
CREATE FUNCTION learning_source.register_legacy() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO learning_source.identities(id,legacy_id) VALUES(NEW.id,NEW.id);
    RETURN NEW;
END $$;
CREATE TRIGGER register_learning_identity AFTER INSERT ON lexicon.entries
FOR EACH ROW EXECUTE FUNCTION learning_source.register_legacy();
INSERT INTO learning_source.identities(id,legacy_id) SELECT id,id FROM lexicon.entries;
ALTER TABLE wordlist_items DROP CONSTRAINT wordlist_items_entry_id_fkey,
    ADD CONSTRAINT wordlist_items_source_fkey FOREIGN KEY (entry_id) REFERENCES learning_source.identities ON DELETE RESTRICT;

CREATE TABLE learning_source.reviewed_publications (
    id uuid PRIMARY KEY REFERENCES lexicon_next.publications ON DELETE CASCADE,
    entry_id uuid NOT NULL REFERENCES lexicon_next.entries ON DELETE CASCADE,
    snapshot jsonb NOT NULL,
    UNIQUE(entry_id,id)
);
CREATE INDEX ON learning_source.reviewed_publications(entry_id);
CREATE TABLE learning_source.reviews (
    version bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    entry_id uuid NOT NULL,
    publication_id uuid NOT NULL,
    lifecycle_revision bigint NOT NULL,
    approved boolean NOT NULL,
    actor_id uuid NOT NULL REFERENCES admins,
    decided_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY(entry_id,publication_id) REFERENCES learning_source.reviewed_publications(entry_id,id) ON DELETE CASCADE
);
CREATE INDEX ON learning_source.reviews(entry_id,version DESC);
CREATE TABLE learning_source.next_surfaces (
    publication_id uuid NOT NULL REFERENCES learning_source.reviewed_publications ON DELETE CASCADE,
    surface text NOT NULL,
    PRIMARY KEY(publication_id,surface)
);
CREATE INDEX ON learning_source.next_surfaces(surface text_pattern_ops);
CREATE TABLE learning_source.next_choice_candidates (LIKE lexicon.question_candidate_index INCLUDING ALL);
ALTER TABLE learning_source.next_choice_candidates ADD FOREIGN KEY(entry_id,publication_id)
    REFERENCES learning_source.reviewed_publications(entry_id,id) ON DELETE CASCADE;

CREATE VIEW learning_source.next_entries AS
SELECT e.id, CASE WHEN active.ok THEN r.publication_id END AS current_publication_id,
       CASE WHEN e.status='archived' THEN e.updated_at END AS archived_at,
       e.lifecycle_revision, r.version AS wordlist_archive_generation, active.ok AS teaching_eligible
FROM (SELECT DISTINCT ON (entry_id) entry_id,version,approved,publication_id,lifecycle_revision
      FROM learning_source.reviews ORDER BY entry_id,version DESC) r
JOIN lexicon_next.entries e ON e.id=r.entry_id
CROSS JOIN LATERAL (SELECT e.status='published' AND e.lifecycle_revision=r.lifecycle_revision AND r.approved AS ok) active;

-- Search appearances from both libraries in one ranking. Word lists store stable entry IDs.
CREATE INDEX next_surfaces_spelling_trgm ON learning_source.next_surfaces USING gin(surface gin_trgm_ops);
CREATE VIEW learning_source.search_surfaces AS
SELECT s.entry_id,s.publication_id,s.surface,s.normalized_surface
FROM lexicon.surface_sources s
JOIN lexicon.entries e ON e.id=s.entry_id AND e.current_publication_id=s.publication_id
JOIN lexicon.entry_publications p ON p.id=s.publication_id AND p.entry_id=s.entry_id
WHERE s.language='en' AND s.dialect_scope IN ('uk','us')
  AND s.normalization_version=1 AND s.content_schema_version=3 AND s.source_kind='form_variant'
  AND s.content_scope='current_publication' AND NOT s.is_deleted AND e.archived_at IS NULL AND p.content_schema_version=3
UNION ALL
SELECT p.entry_id,p.id,n.surface,n.surface FROM learning_source.next_surfaces n
JOIN learning_source.reviewed_publications p ON p.id=n.publication_id
JOIN learning_source.next_entries e ON e.id=p.entry_id AND e.current_publication_id=p.id;

CREATE VIEW learning_source.entries AS
SELECT id,current_publication_id,archived_at,lifecycle_revision,wordlist_archive_generation,teaching_eligible FROM lexicon.entries
UNION ALL SELECT * FROM learning_source.next_entries;

CREATE VIEW learning_source.publications AS
SELECT id,entry_id,snapshot,content_schema_version,publication_number,false AS is_next FROM lexicon.entry_publications
UNION ALL
SELECT s.id,s.entry_id,s.snapshot,3::smallint,p.lifecycle_revision,true FROM learning_source.reviewed_publications s
JOIN lexicon_next.publications p ON p.id=s.id;

-- Join within each origin so lookup by a single entry never scans the other publication table.
CREATE VIEW learning_source.current_publications AS
SELECT e.id AS entry_id,p.id AS publication_id,p.snapshot,e.wordlist_archive_generation,e.teaching_eligible,false AS is_next
FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id
WHERE e.archived_at IS NULL AND p.content_schema_version=3
UNION ALL
SELECT e.id,p.id,p.snapshot,e.wordlist_archive_generation,true,true FROM learning_source.next_entries e
JOIN learning_source.reviewed_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id;

CREATE VIEW learning_source.choice_candidates AS
SELECT c.*,e.teaching_eligible,e.archived_at
FROM lexicon.question_candidate_index c JOIN lexicon.entries e ON e.id=c.entry_id AND e.current_publication_id=c.publication_id
UNION ALL
SELECT c.*,e.teaching_eligible,e.archived_at
FROM learning_source.next_choice_candidates c JOIN learning_source.next_entries e ON e.id=c.entry_id AND e.current_publication_id=c.publication_id;

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
 AND (EXISTS(
   SELECT 1 FROM lexicon.question_candidate_index i JOIN lexicon.entries e
     ON e.id=i.entry_id AND e.current_publication_id=i.publication_id
   WHERE lexicon.question_candidate_key_v1(i.spelling)=c.normalized_term
 ) OR EXISTS(
   SELECT 1 FROM learning_source.next_choice_candidates i JOIN learning_source.next_entries e
     ON e.id=i.entry_id AND e.current_publication_id=i.publication_id
   WHERE lexicon.question_candidate_key_v1(i.spelling)=c.normalized_term
 ));
$$;
