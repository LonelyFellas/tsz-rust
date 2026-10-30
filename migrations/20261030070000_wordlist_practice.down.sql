DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM learning_cycles WHERE generation_version='wordlist_all_v1' OR grading_version='wordlist_mixed_v1') OR EXISTS(SELECT 1 FROM learning_runs WHERE generation_version='wordlist_all_v1' OR grading_version='wordlist_mixed_v1') THEN
  RAISE EXCEPTION 'new learning generation facts exist; refusing incompatible rollback';
 END IF;
 IF EXISTS(SELECT 1 FROM learning_questions WHERE custom_item_id IS NOT NULL) OR EXISTS(SELECT 1 FROM learning_cycle_units WHERE custom_item_id IS NOT NULL) THEN
  RAISE EXCEPTION 'custom learning facts exist; refusing destructive rollback';
 END IF;
END $$;
ALTER TABLE learning_questions
 DROP CONSTRAINT learning_questions_source_identity,
 DROP COLUMN custom_item_id,
 ALTER COLUMN entry_id SET NOT NULL,
 ALTER COLUMN entry_archive_generation SET NOT NULL,
 ALTER COLUMN publication_id SET NOT NULL,
 ALTER COLUMN pos_id SET NOT NULL,
 ALTER COLUMN sense_id SET NOT NULL,
 ALTER COLUMN definition_id SET NOT NULL;
ALTER TABLE learning_cycle_units
 DROP CONSTRAINT learning_cycle_units_source_identity,
 DROP COLUMN custom_item_id,
 ALTER COLUMN entry_id SET NOT NULL,
 ALTER COLUMN entry_archive_generation SET NOT NULL,
 ALTER COLUMN publication_id SET NOT NULL,
 ALTER COLUMN pos_id SET NOT NULL,
 ALTER COLUMN sense_id SET NOT NULL,
 ALTER COLUMN definition_id SET NOT NULL;
