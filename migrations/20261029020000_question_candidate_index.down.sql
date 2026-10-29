-- Only derived search data is removed. Publications, questions and learning facts remain intact.
DROP TRIGGER question_candidates_activation ON lexicon.entries;
DROP TRIGGER question_candidates_publication ON lexicon.entry_publications;
DROP FUNCTION lexicon.ensure_question_candidates_on_activation();
DROP FUNCTION lexicon.refresh_question_candidates_on_publication();
DROP FUNCTION lexicon.refresh_question_candidates(uuid);
DROP VIEW lexicon.question_candidate_projection;
DROP TABLE lexicon.question_candidate_index;
DROP FUNCTION lexicon.question_candidate_key_v1(text);
DROP FUNCTION lexicon.question_candidate_trim_v1(text);
