-- Restoring a binary must not silently restore archived approvals or cancelled work.
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM learning_cycles WHERE generation_version='choice_algorithm_v1') THEN
  RAISE EXCEPTION 'on-demand learning snapshots exist; rollback requires an explicit compatibility plan';
 END IF;
END $$;
