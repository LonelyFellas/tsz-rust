-- Old workers cannot preserve batch admission, queue TTL, or cancellation ownership.
-- Hold the DDL lock while checking so no task can become active between the guard and ALTER.
DO $$ BEGIN
 LOCK TABLE question_generations IN ACCESS EXCLUSIVE MODE;
 IF EXISTS (
  SELECT 1 FROM question_generations
  WHERE batch_id IS NOT NULL AND (
   state IN ('queued','running') OR
   (state IN ('cancelled','failed') AND submitted_at IS NOT NULL AND NOT cancel_delivered)
  )
 ) THEN
  RAISE EXCEPTION 'batch generation work remains; drain batches and confirm remote cancellations before rollback';
 END IF;
END $$;
DROP INDEX question_generations_coverage;
ALTER TABLE question_generations DROP COLUMN batch_id;
DROP TABLE question_generation_batches;
