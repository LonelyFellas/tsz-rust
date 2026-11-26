SELECT entry_id
FROM learning_source.search_surfaces
WHERE normalized_surface COLLATE "C">=$1 AND normalized_surface COLLATE "C"<$4
  AND NOT(entry_id=ANY($2::uuid[]))
ORDER BY normalized_surface COLLATE "C",entry_id LIMIT $3
