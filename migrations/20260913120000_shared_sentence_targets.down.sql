DO $$ BEGIN
  IF EXISTS(SELECT 1 FROM lexicon.shared_sentence_annotations WHERE target_ref IS NOT NULL) THEN
    RAISE EXCEPTION 'shared sentence sense targets exist; refuse destructive rollback';
  END IF;
END $$;
DROP INDEX lexicon.shared_sentence_annotation_sense_idx;
ALTER TABLE lexicon.shared_sentence_annotations
  DROP CONSTRAINT shared_sentence_target_ref_check,
  DROP CONSTRAINT shared_sentence_target_sense_fkey,
  DROP COLUMN target_ref,
  DROP COLUMN target_sense_id;
