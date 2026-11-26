SELECT DISTINCT entry_id
FROM learning_source.search_surfaces
WHERE normalized_surface=$1 AND NOT(entry_id=ANY($2::uuid[]))
ORDER BY entry_id LIMIT $3
