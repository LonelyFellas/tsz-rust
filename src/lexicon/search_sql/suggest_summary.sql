-- Only the selected (at most three) current publications are inspected. Never
-- transfer full publication JSON, forms, sentences or administrative data.
SELECT e.id AS entry_id,p.id AS publication_id,
       p.snapshot#>>'{presentation,label}' AS label,
       ARRAY(
           SELECT left(gloss.text,200)
           FROM jsonb_array_elements(p.snapshot#>'{meanings,pos}') WITH ORDINALITY AS pos(value,n)
           CROSS JOIN LATERAL jsonb_array_elements(pos.value->'senses') WITH ORDINALITY AS sense(value,n)
           CROSS JOIN LATERAL (
               SELECT definition.value#>>'{content,text}' AS text
               FROM jsonb_array_elements(sense.value->'definitions') WITH ORDINALITY AS definition(value,n)
               WHERE definition.value->>'definition_mode' IN ('zh_definition','zh_sentence')
                 AND btrim(definition.value#>>'{content,text}')<>''
               ORDER BY definition.n LIMIT 1
           ) gloss
           ORDER BY pos.n,sense.n LIMIT 2
       ) AS glosses,
       (SELECT COALESCE(sum(jsonb_array_length(pos.value->'senses')),0)::bigint
        FROM jsonb_array_elements(p.snapshot#>'{meanings,pos}') pos(value)) AS sense_count
FROM lexicon.entries e
JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id
WHERE e.id=ANY($1) AND e.archived_at IS NULL AND p.content_schema_version=3
ORDER BY array_position($1,e.id)
