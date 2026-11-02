DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM learning_tasks WHERE choice_policy='mixed_tiered_v1' OR direct_practice_key IS NOT NULL)
    OR EXISTS(SELECT 1 FROM learning_plan_reuse_receipts) THEN
  RAISE EXCEPTION 'practice configuration records exist; retain a compatible backend';
 END IF;
END $$;
DROP TABLE learning_plan_reuse_receipts;
DROP INDEX learning_tasks_active_direct_practice_key;
ALTER TABLE learning_tasks DROP COLUMN direct_practice_key;
ALTER TABLE learning_tasks DROP CONSTRAINT learning_tasks_check1;
ALTER TABLE learning_tasks ADD CONSTRAINT learning_tasks_check1
 CHECK (choice_policy IS NULL OR (choice_policy='strict_tiered_v1' AND question_type='choice_zh_to_en'));
