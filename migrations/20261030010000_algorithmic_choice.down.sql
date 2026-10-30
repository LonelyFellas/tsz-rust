DO $$ BEGIN
 IF EXISTS (SELECT 1 FROM question_generations WHERE generation_method='algorithm') THEN
  RAISE EXCEPTION 'algorithmic questions exist; rollback requires an explicit data migration';
 END IF;
END $$;
ALTER TABLE question_generations DROP COLUMN generation_method;
