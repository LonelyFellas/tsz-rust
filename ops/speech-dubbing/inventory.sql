-- Read-only side/version inventory, usable before or after 20261031090000.
-- No automatic pairing is inferred.
-- Run with a read-only role, e.g.:
-- PGOPTIONS='-c default_transaction_read_only=on' psql -X -v ON_ERROR_STOP=1 --single-transaction "$DATABASE_URL" -f ops/speech-dubbing/inventory.sql
WITH groups AS (
    SELECT value AS g, ordinality AS position
    FROM speech.dubbing_groups, jsonb_array_elements(groups) WITH ORDINALITY
), sides AS (
    SELECT g->>'id' AS group_id, side, item
    FROM groups CROSS JOIN (VALUES ('uk'), ('us')) AS s(side)
    CROSS JOIN LATERAL jsonb_array_elements(g->side) AS item
), repeated AS (
    SELECT group_id, side, item->>'voice_alias' AS alias,
           count(*) AS versions, count(DISTINCT item->'parameters') AS parameter_variants
    FROM sides GROUP BY group_id, side, item->>'voice_alias' HAVING count(*) > 1
)
SELECT g->>'id' AS group_id, g->>'name' AS group_name, position,
       jsonb_array_length(g->'uk') AS uk_count,
       jsonb_array_length(g->'us') AS us_count,
       jsonb_array_length(g->'uk') <> jsonb_array_length(g->'us') AS unequal_sides,
       (SELECT count(*) FROM sides WHERE group_id=g->>'id' AND item->>'selected'='false') AS unselected_count,
       COALESCE((SELECT jsonb_agg(jsonb_build_object('side',side,'voice_alias',alias,'versions',versions,'parameter_variants',parameter_variants)) FROM repeated WHERE group_id=g->>'id'),'[]'::jsonb) AS repeated_voices,
       COALESCE((SELECT jsonb_agg(jsonb_build_object('side',side,'id',item->>'id','name',item->>'name')) FROM sides WHERE group_id=g->>'id'),'[]'::jsonb) AS side_identities
FROM groups
ORDER BY position;
