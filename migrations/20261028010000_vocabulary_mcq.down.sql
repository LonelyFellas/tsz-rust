-- Refuse rollback once choice learning evidence exists. Export/retention is a separate decision.
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM learning_tasks WHERE question_type='choice_zh_to_en') THEN
  RAISE EXCEPTION 'choice learning evidence exists; rollback requires an explicit data migration';
 END IF;
END $$;
ALTER TABLE learning_completions DROP CONSTRAINT learning_completions_question_type_check;
ALTER TABLE learning_completions ADD CONSTRAINT learning_completions_question_type_check CHECK(question_type='spelling_zh_to_en');
ALTER TABLE learning_runs DROP CONSTRAINT learning_runs_question_type_check;
ALTER TABLE learning_runs ADD CONSTRAINT learning_runs_question_type_check CHECK(question_type='spelling_zh_to_en');
ALTER TABLE learning_tasks DROP CONSTRAINT learning_tasks_question_type_check;
ALTER TABLE learning_tasks ADD CONSTRAINT learning_tasks_question_type_check CHECK(question_type='spelling_zh_to_en');
DROP TABLE question_review_deliveries;
DROP TABLE question_bank;
DROP TABLE question_generations;
ALTER TABLE lexicon.entries DROP COLUMN teaching_eligible;
