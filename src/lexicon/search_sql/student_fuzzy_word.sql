SELECT entry_id,publication_id
FROM learning_source.search_surfaces
WHERE NOT(entry_id=ANY($2::uuid[])) AND surface % $1
  AND abs(char_length(surface)-char_length($1))<=greatest(2,char_length($1)/3)
GROUP BY entry_id,publication_id ORDER BY max(similarity(surface,$1)) DESC,entry_id LIMIT $3
