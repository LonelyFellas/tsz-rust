DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM speech.dubbing_groups, jsonb_array_elements(groups) g
        WHERE (g->>'is_global')::boolean AND
            (jsonb_array_length(g->'uk') > 0 OR jsonb_array_length(g->'us') > 0 OR (g->>'revision')::bigint > 0)
    ) OR EXISTS (
        SELECT 1 FROM speech.dubbing_groups, jsonb_array_elements(groups) g
        WHERE NOT (g->>'is_global')::boolean AND
            (jsonb_array_length(g->'areas') = 0 OR jsonb_array_length(g->'uk') = 0 OR jsonb_array_length(g->'us') = 0)
    ) OR EXISTS (
        SELECT 1 FROM speech.dubbing_groups, jsonb_array_elements(groups) g,
            jsonb_array_elements((g->'uk') || (g->'us')) r
        WHERE NOT (r->>'selected')::boolean
    ) THEN
        RAISE EXCEPTION 'Cannot roll back global or unselected dubbing recommendations without losing saved data';
    END IF;
END $$;
UPDATE speech.dubbing_groups SET groups = COALESCE((
    SELECT jsonb_agg((g - 'revision' - 'is_global') || jsonb_build_object(
        'uk', COALESCE((SELECT jsonb_agg(r - 'selected' ORDER BY n) FROM jsonb_array_elements(g->'uk') WITH ORDINALITY rec(r,n)), '[]'::jsonb),
        'us', COALESCE((SELECT jsonb_agg(r - 'selected' ORDER BY n) FROM jsonb_array_elements(g->'us') WITH ORDINALITY rec(r,n)), '[]'::jsonb)
    ) ORDER BY position)
    FROM jsonb_array_elements(groups) WITH ORDINALITY entries(g,position)
    WHERE NOT (g->>'is_global')::boolean
), '[]'::jsonb);
