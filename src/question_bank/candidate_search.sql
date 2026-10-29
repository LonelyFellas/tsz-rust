-- Publication writes maintain the derived index; authoritative hydration still checks the returned page.
WITH matches AS (
 SELECT c.entry_id,c.publication_id,c.pos_id,c.sense_id,c.definition_id,c.form_id,
        CASE WHEN $9::text IS NULL AND $8::text IS NULL THEN 0::real ELSE greatest(similarity(lower(c.spelling),lower(COALESCE($9,''))),similarity(c.definition,COALESCE($8,''))) END AS relevance
 FROM lexicon.question_candidate_index c
 JOIN lexicon.entries e ON e.id=c.entry_id AND e.current_publication_id=c.publication_id
 WHERE e.archived_at IS NULL AND e.teaching_eligible
   AND ($1::uuid IS NULL OR e.id=$1) AND ($2::text IS NULL OR c.spelling ILIKE $2 OR c.definition ILIKE $2)
   AND c.level=ANY($3::text[]) AND c.answer_profile=$4::text
   AND ($5::text IS NULL OR c.part_of_speech=$5) AND NOT(e.id=ANY($6::uuid[]))
   AND NOT(c.accepted_keys && $7::text[]) AND ($8::text IS NULL OR NOT(c.definition_keys @> ARRAY[$8]))
   AND (NOT $10::boolean OR e.created_by_admin_id=$11::uuid)
)
