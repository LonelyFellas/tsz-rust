ALTER TABLE learning_tasks
 ADD COLUMN generation_policy text NOT NULL DEFAULT 'legacy_v1' CHECK(generation_policy IN ('legacy_v1','coverage_v1')),
 ADD COLUMN start_date date,
 ADD COLUMN end_date date,
 ADD CONSTRAINT learning_plan_schedule CHECK (
   (start_date IS NULL AND end_date IS NULL) OR
   (generation_policy='coverage_v1' AND task_type='daily' AND start_date IS NOT NULL AND end_date IS NOT NULL AND end_date>=start_date)),
 ADD CONSTRAINT learning_plan_name CHECK(generation_policy='legacy_v1' OR char_length(name)<=50);
CREATE TABLE learning_cycles (
 id uuid PRIMARY KEY, task_id uuid NOT NULL, user_id uuid NOT NULL,
 cycle_number integer NOT NULL CHECK(cycle_number>0),
 state text NOT NULL CHECK(state IN ('active','completed','cancelled')),
 total_count integer NOT NULL CHECK(total_count BETWEEN 1 AND 2000),
 settings_snapshot jsonb NOT NULL, generation_version text NOT NULL, grading_version text NOT NULL,
 after_cycle_id uuid, earliest_day date NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), completed_at timestamptz,
 FOREIGN KEY(task_id,user_id) REFERENCES learning_tasks(id,user_id) ON DELETE CASCADE,
 UNIQUE(id,task_id,user_id), UNIQUE(task_id,cycle_number),
 FOREIGN KEY(after_cycle_id,task_id,user_id) REFERENCES learning_cycles(id,task_id,user_id) ON DELETE CASCADE,
 CHECK((state='completed')=(completed_at IS NOT NULL))
);
CREATE UNIQUE INDEX learning_cycle_active ON learning_cycles(task_id) WHERE state='active';
CREATE TABLE learning_cycle_units (
 id uuid PRIMARY KEY, cycle_id uuid NOT NULL REFERENCES learning_cycles(id) ON DELETE CASCADE,
 position integer NOT NULL CHECK(position>=0), unit_key text NOT NULL,
 source_wordlist_id uuid NOT NULL, source_revision bigint NOT NULL,
 source_membership_id uuid NOT NULL, source_public_generation bigint,
 entry_id uuid NOT NULL, entry_archive_generation bigint NOT NULL, publication_id uuid NOT NULL,
 pos_id uuid NOT NULL, sense_id uuid NOT NULL, definition_id uuid NOT NULL, form_ids uuid[] NOT NULL,
 prompt_snapshot jsonb NOT NULL, answer_snapshot jsonb NOT NULL, fingerprint bytea NOT NULL,
 covered_answer_id uuid,
 UNIQUE(cycle_id,position), UNIQUE(cycle_id,unit_key), UNIQUE(id,cycle_id)
);
CREATE INDEX learning_cycle_pending ON learning_cycle_units(cycle_id,position) WHERE covered_answer_id IS NULL;
ALTER TABLE learning_runs ADD COLUMN cycle_id uuid,
 ADD CONSTRAINT learning_run_cycle FOREIGN KEY(cycle_id,task_id,user_id) REFERENCES learning_cycles(id,task_id,user_id) ON DELETE CASCADE,
 ADD CONSTRAINT learning_run_cycle_pair UNIQUE(id,cycle_id);
ALTER TABLE learning_questions ADD COLUMN cycle_id uuid, ADD COLUMN cycle_unit_id uuid,
 ADD CONSTRAINT learning_question_cycle_pair CHECK((cycle_id IS NULL)=(cycle_unit_id IS NULL)),
 ADD CONSTRAINT learning_question_cycle_run FOREIGN KEY(run_id,cycle_id) REFERENCES learning_runs(id,cycle_id) ON DELETE CASCADE,
 ADD CONSTRAINT learning_question_unit FOREIGN KEY(cycle_unit_id,cycle_id) REFERENCES learning_cycle_units(id,cycle_id) ON DELETE CASCADE,
 ADD CONSTRAINT learning_question_answer_unit UNIQUE(id,run_id,cycle_unit_id);
ALTER TABLE learning_answers ADD COLUMN cycle_unit_id uuid,
 ADD CONSTRAINT learning_answer_unit FOREIGN KEY(question_id,run_id,cycle_unit_id) REFERENCES learning_questions(id,run_id,cycle_unit_id) ON DELETE CASCADE,
 ADD CONSTRAINT learning_answer_unit_pair UNIQUE(id,cycle_unit_id);
CREATE UNIQUE INDEX learning_unit_first_answer ON learning_answers(cycle_unit_id) WHERE cycle_unit_id IS NOT NULL;
ALTER TABLE learning_cycle_units ADD CONSTRAINT learning_covered_answer FOREIGN KEY(covered_answer_id,id) REFERENCES learning_answers(id,cycle_unit_id) DEFERRABLE INITIALLY DEFERRED;
CREATE TABLE learning_cycle_requests (
 user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE, idempotency_key uuid NOT NULL,
 request_hash bytea NOT NULL, task_id uuid NOT NULL, after_cycle_id uuid NOT NULL,
 operation text NOT NULL CHECK(operation IN ('next','restart_blocked')), result_cycle_id uuid NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(), PRIMARY KEY(user_id,idempotency_key),
 FOREIGN KEY(result_cycle_id,task_id,user_id) REFERENCES learning_cycles(id,task_id,user_id) ON DELETE CASCADE,
 FOREIGN KEY(after_cycle_id,task_id,user_id) REFERENCES learning_cycles(id,task_id,user_id) ON DELETE CASCADE
);
-- Do not allow a legacy run to be attached to a coverage task (or conversely).
CREATE FUNCTION check_learning_run_policy() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM learning_tasks WHERE id=NEW.task_id AND ((generation_policy='coverage_v1')<>(NEW.cycle_id IS NOT NULL))) THEN
  RAISE EXCEPTION 'learning run generation policy mismatch' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER learning_run_policy BEFORE INSERT OR UPDATE ON learning_runs FOR EACH ROW EXECUTE FUNCTION check_learning_run_policy();
CREATE FUNCTION check_learning_question_policy() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_TABLE_NAME='learning_questions' THEN
  IF NOT EXISTS(SELECT 1 FROM learning_runs WHERE id=NEW.run_id AND cycle_id IS NOT DISTINCT FROM NEW.cycle_id) THEN
   RAISE EXCEPTION 'question cycle mismatch' USING ERRCODE='23514';
  END IF;
 ELSE
  IF NOT EXISTS(SELECT 1 FROM learning_questions WHERE id=NEW.question_id AND cycle_unit_id IS NOT DISTINCT FROM NEW.cycle_unit_id) THEN
   RAISE EXCEPTION 'answer cycle unit mismatch' USING ERRCODE='23514';
  END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER learning_question_policy BEFORE INSERT OR UPDATE ON learning_questions FOR EACH ROW EXECUTE FUNCTION check_learning_question_policy();
CREATE TRIGGER learning_answer_policy BEFORE INSERT OR UPDATE ON learning_answers FOR EACH ROW EXECUTE FUNCTION check_learning_question_policy();
