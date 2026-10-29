-- Derived publication search data only. A one-time backfill is allowed to exceed API query timeouts.
SET LOCAL statement_timeout = '5min';

CREATE FUNCTION lexicon.question_candidate_trim_v1(value text) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
 SELECT btrim(value,U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')
$$;
CREATE FUNCTION lexicon.question_candidate_key_v1(value text) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
 SELECT lower(replace(translate(
   btrim(regexp_replace(normalize(value,NFKC),U&'[\0009-\000D\0020\0085\00A0\1680\2000-\200A\2028\2029\202F\205F\3000]+',' ','g')),
   U&'\2018\2019\02bc\2010\2011\2012\2013\2014\2212',repeat(chr(39),3)||'------'
 ),U&'\0130',U&'i\0307'))
$$;

CREATE TABLE lexicon.question_candidate_index (
 publication_id uuid NOT NULL REFERENCES lexicon.entry_publications(id) ON DELETE CASCADE,
 entry_id uuid NOT NULL REFERENCES lexicon.entries(id) ON DELETE CASCADE,
 pos_id uuid NOT NULL, sense_id uuid NOT NULL, definition_id uuid NOT NULL, form_id uuid NOT NULL,
 answer_profile text NOT NULL CHECK(answer_profile IN ('BrE','AmE')),
 level text NOT NULL CHECK(level IN ('A1','A2','B1','B2','C1','C2')),
 part_of_speech text NOT NULL, spelling text NOT NULL, definition text NOT NULL,
 accepted_keys text[] NOT NULL, definition_keys text[] NOT NULL,
 PRIMARY KEY(publication_id,answer_profile,pos_id,sense_id,definition_id,form_id)
);
CREATE INDEX question_candidate_entry ON lexicon.question_candidate_index(entry_id);
CREATE INDEX question_candidate_context ON lexicon.question_candidate_index(answer_profile,entry_id,pos_id,sense_id,definition_id,form_id) INCLUDE(publication_id,level);
CREATE INDEX question_candidate_spelling ON lexicon.question_candidate_index USING gin(spelling gin_trgm_ops);
CREATE INDEX question_candidate_definition ON lexicon.question_candidate_index USING gin(definition gin_trgm_ops);

-- A shared projection expression keeps initial backfill, publication writes and repairs identical.
-- NOT MATERIALIZED lets a single-publication refresh push its ID into both bounded branches.
CREATE VIEW lexicon.question_candidate_projection AS
WITH bound_forms AS NOT MATERIALIZED (
 SELECT p.entry_id,p.id AS publication_id,p.snapshot,fp.value AS pos,s.value AS sense,resolved.value AS form
 FROM lexicon.entry_publications p
 CROSS JOIN LATERAL jsonb_array_elements(p.snapshot#>'{forms,pos}') fp(value)
 CROSS JOIN LATERAL jsonb_array_elements(p.snapshot#>'{meanings,pos}') mp(value)
 CROSS JOIN LATERAL jsonb_array_elements(mp.value->'senses') s(value)
 CROSS JOIN LATERAL jsonb_array_elements(fp.value->'forms') f(value)
 CROSS JOIN LATERAL (
   SELECT CASE WHEN p.snapshot#>>'{forms,forms_format}'='shared_v1' THEN
     jsonb_build_object('id',f.value->'id','form_type',f.value->'form_type','regional_variants',(
       SELECT content.value->'regional_variants' FROM jsonb_array_elements(p.snapshot#>'{forms,contents}') content(value)
       WHERE content.value->>'id'=f.value->>'content_id'
     )) ELSE f.value END AS value
 ) resolved
 CROSS JOIN LATERAL (
   SELECT g.value FROM jsonb_array_elements(fp.value->'form_groups') WITH ORDINALITY g(value,ord)
   WHERE EXISTS(SELECT 1 FROM jsonb_array_elements(g.value->'members') m(value) WHERE m.value->>'form_id'=f.value->>'id')
   ORDER BY g.ord LIMIT 1
 ) fg
 WHERE p.content_schema_version=3 AND fp.value->>'pos_id'=mp.value->>'pos_id'
   AND f.value->>'form_type'='base' AND NOT COALESCE((s.value->>'depends_on_context')::boolean,false)
   AND (fg.value->>'scope'='general' OR CASE WHEN jsonb_typeof(s.value->'form_group_ids')='array'
       THEN (s.value->'form_group_ids') ? (fg.value->>'id') ELSE s.value->>'form_group_id'=fg.value->>'id' END)
), accepted AS NOT MATERIALIZED (
 SELECT publication_id,pos->>'pos_id' AS pos_id,sense->>'id' AS sense_id,
        count(DISTINCT lexicon.question_candidate_trim_v1(v.spelling)) FILTER(WHERE lexicon.question_candidate_trim_v1(v.spelling)<>'') AS raw_count,
        array_agg(DISTINCT lexicon.question_candidate_key_v1(v.spelling)) FILTER(WHERE lexicon.question_candidate_trim_v1(v.spelling)<>'') AS keys
 FROM bound_forms CROSS JOIN LATERAL (VALUES(form#>>'{regional_variants,common,spelling}'),(form#>>'{regional_variants,uk,spelling}'),(form#>>'{regional_variants,us,spelling}')) v(spelling)
 GROUP BY publication_id,pos->>'pos_id',sense->>'id'
), definitions AS NOT MATERIALIZED (
 SELECT p.id AS publication_id,count(*) AS definition_count,array_agg(DISTINCT lexicon.question_candidate_key_v1(d.value#>>'{content,text}')) AS keys
 FROM lexicon.entry_publications p
 CROSS JOIN LATERAL jsonb_array_elements(p.snapshot#>'{meanings,pos}') mp(value)
 CROSS JOIN LATERAL jsonb_array_elements(mp.value->'senses') s(value)
 CROSS JOIN LATERAL jsonb_array_elements(s.value->'definitions') d(value)
 WHERE p.content_schema_version=3 AND d.value->>'definition_mode'='zh_definition' AND lexicon.question_candidate_trim_v1(d.value#>>'{content,text}')<>''
 GROUP BY p.id
)
SELECT b.publication_id,b.entry_id,(b.pos->>'pos_id')::uuid AS pos_id,(b.sense->>'id')::uuid AS sense_id,
       (d.value->>'id')::uuid AS definition_id,(b.form->>'id')::uuid AS form_id,v.profile AS answer_profile,
       d.value->>'level' AS level,lexicon.question_candidate_trim_v1(b.pos->>'pos') AS part_of_speech,
       lexicon.question_candidate_trim_v1(COALESCE(b.form#>>'{regional_variants,common,spelling}',b.form#>>ARRAY['regional_variants',v.side,'spelling'])) AS spelling,
       lexicon.question_candidate_trim_v1(d.value#>>'{content,text}') AS definition,
       COALESCE(a.keys,'{}'::text[]) AS accepted_keys,all_def.keys AS definition_keys
FROM bound_forms b
JOIN accepted a ON a.publication_id=b.publication_id AND a.pos_id=b.pos->>'pos_id' AND a.sense_id=b.sense->>'id'
JOIN definitions all_def ON all_def.publication_id=b.publication_id
CROSS JOIN LATERAL jsonb_array_elements(b.sense->'definitions') d(value)
CROSS JOIN (VALUES('BrE','uk'),('AmE','us')) v(profile,side)
WHERE d.value->>'definition_mode'='zh_definition' AND d.value->>'level'=ANY(ARRAY['A1','A2','B1','B2','C1','C2'])
  AND lexicon.question_candidate_trim_v1(d.value#>>'{content,text}')<>'' AND a.raw_count<=30 AND all_def.definition_count<=101;

CREATE FUNCTION lexicon.refresh_question_candidates(publication uuid) RETURNS void
LANGUAGE sql SET jit=off AS $$
 DELETE FROM lexicon.question_candidate_index WHERE publication_id=publication;
 INSERT INTO lexicon.question_candidate_index SELECT * FROM lexicon.question_candidate_projection WHERE publication_id=publication;
$$;
CREATE FUNCTION lexicon.refresh_question_candidates_on_publication() RETURNS trigger
LANGUAGE plpgsql SET jit=off AS $$
BEGIN
 PERFORM lexicon.refresh_question_candidates(NEW.id);
 RETURN NEW;
END $$;
CREATE TRIGGER question_candidates_publication AFTER INSERT OR UPDATE OF snapshot,content_schema_version ON lexicon.entry_publications
 FOR EACH ROW EXECUTE FUNCTION lexicon.refresh_question_candidates_on_publication();
CREATE FUNCTION lexicon.ensure_question_candidates_on_activation() RETURNS trigger
LANGUAGE plpgsql SET jit=off AS $$
BEGIN
 IF NEW.current_publication_id IS NULL THEN RETURN NEW; END IF;
 IF TG_OP='UPDATE' AND NEW.current_publication_id IS NOT DISTINCT FROM OLD.current_publication_id THEN RETURN NEW; END IF;
 IF NOT EXISTS(SELECT 1 FROM lexicon.question_candidate_index WHERE publication_id=NEW.current_publication_id) THEN
  PERFORM lexicon.refresh_question_candidates(NEW.current_publication_id);
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER question_candidates_activation AFTER INSERT OR UPDATE OF current_publication_id ON lexicon.entries
 FOR EACH ROW EXECUTE FUNCTION lexicon.ensure_question_candidates_on_activation();

-- Backfill active publications once; historical publications are indexed if reactivated.
INSERT INTO lexicon.question_candidate_index
SELECT projection.* FROM lexicon.question_candidate_projection projection
JOIN lexicon.entries e ON e.id=projection.entry_id AND e.current_publication_id=projection.publication_id;
ANALYZE lexicon.question_candidate_index;
