-- Keep immutable question/learning evidence, retire only the unused generation workflow.
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM question_generations WHERE agent_run_id IS NOT NULL AND state IN ('queued','running')) THEN
  RAISE EXCEPTION 'drain remote Agent executions before retiring question generation';
 END IF;
END $$;
UPDATE question_generations SET state='cancelled',revision=revision+1,updated_at=now() WHERE state IN ('queued','running');
UPDATE question_bank SET state='disabled',revision=revision+1,updated_at=now() WHERE state IN ('draft','approved');
