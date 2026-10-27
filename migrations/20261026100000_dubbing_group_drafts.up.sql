UPDATE speech.dubbing_groups
SET groups = jsonb_build_array(jsonb_build_object(
    'id', '00000000-0000-4000-8000-000000000001', 'name', '全局常用',
    'areas', '[]'::jsonb, 'uk', '[]'::jsonb, 'us', '[]'::jsonb,
    'revision', 0, 'is_global', true
)) || COALESCE((
    SELECT jsonb_agg(g || jsonb_build_object(
        'revision', 1, 'is_global', false,
        'uk', COALESCE((SELECT jsonb_agg(r || '{"selected":true}'::jsonb ORDER BY n)
            FROM jsonb_array_elements(g->'uk') WITH ORDINALITY AS rec(r,n)), '[]'::jsonb),
        'us', COALESCE((SELECT jsonb_agg(r || '{"selected":true}'::jsonb ORDER BY n)
            FROM jsonb_array_elements(g->'us') WITH ORDINALITY AS rec(r,n)), '[]'::jsonb)
    ) ORDER BY position)
    FROM jsonb_array_elements(groups) WITH ORDINALITY AS entries(g,position)
), '[]'::jsonb);
