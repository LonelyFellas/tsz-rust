ALTER TABLE learning_cycle_requests DROP CONSTRAINT learning_cycle_requests_operation_check;
ALTER TABLE learning_cycle_requests ADD CONSTRAINT learning_cycle_requests_operation_check
 CHECK(operation IN ('next','restart_blocked','refresh_sources'));
