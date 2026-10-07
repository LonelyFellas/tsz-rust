DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM learning_tasks) OR EXISTS(SELECT 1 FROM learning_runs) THEN
  RAISE EXCEPTION 'learning facts exist; refusing destructive rollback';
 END IF;
END $$;
DROP TABLE learning_run_start_requests, learning_completions, learning_answers, learning_questions, learning_runs, learning_tasks;
DROP TRIGGER learning_public_generation ON wordlists;
DROP FUNCTION advance_learning_public_generation();
ALTER TABLE wordlist_items DROP COLUMN learning_membership_id;
ALTER TABLE wordlists DROP COLUMN learning_public_generation;
