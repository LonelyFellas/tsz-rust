ALTER TABLE learning_tasks DROP CONSTRAINT learning_tasks_generation_policy_check;
ALTER TABLE learning_tasks ADD CONSTRAINT learning_tasks_generation_policy_check CHECK(generation_policy IN ('legacy_v1','coverage_v1','mastery_v1'));
ALTER TABLE learning_tasks DROP CONSTRAINT learning_plan_schedule;
ALTER TABLE learning_tasks ADD CONSTRAINT learning_plan_schedule CHECK((start_date IS NULL AND end_date IS NULL) OR (generation_policy IN ('coverage_v1','mastery_v1') AND task_type='daily' AND start_date IS NOT NULL AND end_date IS NOT NULL AND end_date>=start_date));
ALTER TABLE learning_runs ADD COLUMN practice_mode text CHECK(practice_mode='mastery_v1'), ADD CONSTRAINT learning_practice_size CHECK(practice_mode IS NULL OR target_count<=30);
ALTER TABLE learning_cycle_units
 ADD COLUMN practice_success_days smallint NOT NULL DEFAULT 0 CHECK(practice_success_days BETWEEN 0 AND 2),
 ADD COLUMN practice_last_success_day date,
 ADD COLUMN practice_last_day date,
 ADD COLUMN practice_next_day date,
 ADD COLUMN practice_interval_step smallint NOT NULL DEFAULT 0 CHECK(practice_interval_step BETWEEN 0 AND 4);
ALTER TABLE learning_questions
 ADD COLUMN practice_kind text NOT NULL DEFAULT 'new' CHECK(practice_kind IN ('new','review','retry')),
 ADD COLUMN retry_of uuid,
 ADD CONSTRAINT learning_retry_parent FOREIGN KEY(retry_of,run_id) REFERENCES learning_questions(id,run_id) ON DELETE CASCADE,
 ADD CONSTRAINT learning_retry_pair CHECK((practice_kind='retry')=(retry_of IS NOT NULL));
ALTER TABLE learning_questions DROP CONSTRAINT learning_questions_run_id_unit_key_key;
CREATE UNIQUE INDEX learning_question_initial_unit ON learning_questions(run_id,unit_key) WHERE practice_kind<>'retry';
ALTER TABLE learning_answers ADD COLUMN is_repeat boolean NOT NULL DEFAULT false;
DROP INDEX learning_unit_first_answer;
CREATE UNIQUE INDEX learning_unit_first_answer ON learning_answers(cycle_unit_id) WHERE cycle_unit_id IS NOT NULL AND NOT is_repeat;
CREATE OR REPLACE FUNCTION check_learning_run_policy() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM learning_tasks WHERE id=NEW.task_id AND ((generation_policy<>'legacy_v1')<>(NEW.cycle_id IS NOT NULL) OR (generation_policy='mastery_v1')<>(NEW.practice_mode IS NOT NULL))) THEN
  RAISE EXCEPTION 'learning run generation policy mismatch' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE FUNCTION check_learning_practice_answer() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM learning_questions q JOIN learning_runs r ON r.id=q.run_id WHERE q.id=NEW.question_id AND NEW.is_repeat=(q.practice_kind<>'new') AND (q.practice_kind='new' OR r.practice_mode='mastery_v1')) THEN
  RAISE EXCEPTION 'learning repeat answer policy mismatch' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER learning_practice_answer BEFORE INSERT OR UPDATE ON learning_answers FOR EACH ROW EXECUTE FUNCTION check_learning_practice_answer();
