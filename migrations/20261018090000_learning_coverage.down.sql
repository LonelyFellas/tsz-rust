DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM learning_tasks WHERE generation_policy='coverage_v1') THEN
  RAISE EXCEPTION 'coverage learning data exists; lossless rollback refused';
 END IF;
END $$;
DROP TRIGGER learning_question_policy ON learning_questions;
DROP TRIGGER learning_answer_policy ON learning_answers;
DROP FUNCTION check_learning_question_policy();
DROP TRIGGER learning_run_policy ON learning_runs;
DROP FUNCTION check_learning_run_policy();
DROP TABLE learning_cycle_requests;
ALTER TABLE learning_cycle_units DROP CONSTRAINT learning_covered_answer;
ALTER TABLE learning_answers DROP CONSTRAINT learning_answer_unit, DROP CONSTRAINT learning_answer_unit_pair, DROP COLUMN cycle_unit_id;
ALTER TABLE learning_questions DROP CONSTRAINT learning_question_cycle_pair, DROP CONSTRAINT learning_question_cycle_run, DROP CONSTRAINT learning_question_unit, DROP CONSTRAINT learning_question_answer_unit, DROP COLUMN cycle_id, DROP COLUMN cycle_unit_id;
ALTER TABLE learning_runs DROP CONSTRAINT learning_run_cycle, DROP CONSTRAINT learning_run_cycle_pair, DROP COLUMN cycle_id;
DROP TABLE learning_cycle_units;
DROP TABLE learning_cycles;
ALTER TABLE learning_tasks DROP CONSTRAINT learning_plan_schedule, DROP CONSTRAINT learning_plan_name, DROP COLUMN generation_policy, DROP COLUMN start_date, DROP COLUMN end_date;
