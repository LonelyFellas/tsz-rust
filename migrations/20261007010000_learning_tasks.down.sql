DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM learning_tasks) OR EXISTS(SELECT 1 FROM learning_runs) THEN
  RAISE EXCEPTION 'learning facts exist; refusing destructive rollback';
 END IF;
END $$;
DROP TABLE learning_run_start_requests, learning_completions, learning_answers, learning_questions, learning_runs, learning_tasks;
