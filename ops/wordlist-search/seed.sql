-- Run only through benchmark.py seed, in an empty, task-isolated database.
CREATE TEMP TABLE search_bench_rows AS
SELECT n,
       md5('wordlist-search-entry-'||n)::uuid AS entry_id,
       md5('wordlist-search-publication-'||n)::uuid AS publication_id,
       CASE n WHEN 1 THEN 'apple' WHEN 2 THEN 'application' WHEN 3 THEN 'apply'
         WHEN 4 THEN 'banana' WHEN 5 THEN 'an' WHEN 6 THEN 'look forward to'
         WHEN 7 THEN 'a' WHEN 8 THEN 'example'
         ELSE (CASE WHEN n%10=0 THEN 'pre' WHEN n%10=1 THEN 'look forward to ' ELSE '' END)
           ||translate(substr(md5(n::text),1,12),'0123456789','ghijklmnop') END AS label
FROM generate_series(1,250000) AS n;
INSERT INTO lexicon.entries(id,content_schema_version,language,kind,detection_snapshot,created_by_admin_id,updated_by_admin_id)
SELECT b.entry_id,3,'en',CASE WHEN b.label LIKE '% %' THEN 'phrase' ELSE 'word' END,'{}',a.id,a.id
FROM search_bench_rows b CROSS JOIN (SELECT id FROM admins WHERE phone='19900008888') a;
INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id)
SELECT b.publication_id,b.entry_id,1,1,3,
       jsonb_set(jsonb_set(t.snapshot,'{id}',to_jsonb(b.entry_id::text)),
         '{presentation,label}',to_jsonb(b.label)) || jsonb_build_object('annotation',repeat('unrelated large publication annotation ',200)),
       decode(md5(b.entry_id::text),'hex'),a.id
FROM search_bench_rows b CROSS JOIN search_bench_template t
CROSS JOIN (SELECT id FROM admins WHERE phone='19900008888') a;
UPDATE lexicon.entries e SET current_publication_id=b.publication_id FROM search_bench_rows b WHERE e.id=b.entry_id;
INSERT INTO lexicon.surface_sources(entry_id,source_id,source_kind,source_node_id,language,entry_kind,dialect,dialect_scope,surface,normalized_surface,normalization_version,source_revision,content_scope,publication_id,pos_id,pos,form_type,content_schema_version,form_id,variant_id,group_ids,projection_version)
SELECT b.entry_id,b.entry_id||':'||f.suffix,'form_variant',md5(b.entry_id||f.suffix)::uuid,'en',CASE WHEN b.label LIKE '% %' THEN 'phrase' ELSE 'word' END,
       'common',d.scope,b.label||f.suffix,b.label||f.suffix,1,1,'current_publication',b.publication_id,
       b.entry_id,'noun',CASE WHEN f.suffix='' THEN 'base' ELSE 'plural' END,3,b.entry_id,md5(b.entry_id||f.suffix)::uuid,ARRAY[b.entry_id],'search-benchmark-v1'
FROM search_bench_rows b CROSS JOIN (VALUES('uk'),('us')) d(scope)
CROSS JOIN LATERAL (SELECT '' AS suffix UNION ALL SELECT 's' WHERE b.n%5=0) f;
