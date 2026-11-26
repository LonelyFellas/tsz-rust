WITH nearest AS MATERIALIZED (
    SELECT entry_id,surface,surface <-> $1 AS distance
    FROM learning_source.search_surfaces
    WHERE NOT(entry_id=ANY($2::uuid[])) AND char_length(surface)>=6 AND surface % $1
    ORDER BY surface <-> $1 LIMIT 32
)
SELECT entry_id FROM nearest
WHERE abs(char_length(surface)-char_length($1))<=greatest(2,char_length($1)/3)
GROUP BY entry_id ORDER BY min(distance),entry_id LIMIT $3
