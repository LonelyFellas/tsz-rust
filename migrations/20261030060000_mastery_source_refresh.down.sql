DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM learning_cycle_requests WHERE operation='refresh_sources') THEN
  RAISE EXCEPTION 'source refresh receipts exist; rollback requires an explicitly reviewed data strategy';
 END IF;
END $$;
ALTER TABLE learning_cycle_requests DROP CONSTRAINT learning_cycle_requests_operation_check;
ALTER TABLE learning_cycle_requests ADD CONSTRAINT learning_cycle_requests_operation_check
 CHECK(operation IN ('next','restart_blocked'));
