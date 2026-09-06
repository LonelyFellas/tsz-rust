-- Deleted prebindings are intentionally not recreated; recover data from a backup if needed.
ALTER TABLE lexicon.relations DROP CONSTRAINT lexicon_relations_no_prebinding_check;
