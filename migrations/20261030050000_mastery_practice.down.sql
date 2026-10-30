DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM learning_tasks WHERE generation_policy='mastery_v1') THEN
  RAISE EXCEPTION 'mastery practice facts exist; rollback requires an explicitly reviewed data strategy';
 END IF;
END $$;
DROP TRIGGER learning_practice_answer ON learning_answers;
DROP FUNCTION check_learning_practice_answer();
DROP INDEX learning_unit_first_answer;
ALTER TABLE learning_answers DROP COLUMN is_repeat;
CREATE UNIQUE INDEX learning_unit_first_answer ON learning_answers(cycle_unit_id) WHERE cycle_unit_id IS NOT NULL;
DROP INDEX learning_question_initial_unit;
ALTER TABLE learning_questions ADD UNIQUE(run_id,unit_key);
ALTER TABLE learning_questions DROP CONSTRAINT learning_retry_parent, DROP CONSTRAINT learning_retry_pair, DROP COLUMN retry_of, DROP COLUMN practice_kind;
ALTER TABLE learning_cycle_units DROP COLUMN practice_success_days, DROP COLUMN practice_last_success_day, DROP COLUMN practice_last_day, DROP COLUMN practice_next_day, DROP COLUMN practice_interval_step;
ALTER TABLE learning_runs DROP CONSTRAINT learning_practice_size, DROP COLUMN practice_mode;
ALTER TABLE learning_tasks DROP CONSTRAINT learning_plan_schedule;
ALTER TABLE learning_tasks ADD CONSTRAINT learning_plan_schedule CHECK((start_date IS NULL AND end_date IS NULL) OR (generation_policy='coverage_v1' AND task_type='daily' AND start_date IS NOT NULL AND end_date IS NOT NULL AND end_date>=start_date));
ALTER TABLE learning_tasks DROP CONSTRAINT learning_tasks_generation_policy_check;
ALTER TABLE learning_tasks ADD CONSTRAINT learning_tasks_generation_policy_check CHECK(generation_policy IN ('legacy_v1','coverage_v1'));
CREATE OR REPLACE FUNCTION check_learning_run_policy() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM learning_tasks WHERE id=NEW.task_id AND ((generation_policy='coverage_v1')<>(NEW.cycle_id IS NOT NULL))) THEN
  RAISE EXCEPTION 'learning run generation policy mismatch' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
