ALTER TABLE coin_operations ADD COLUMN reverses_operation_id UUID REFERENCES coin_operations(id) ON DELETE RESTRICT;
ALTER TABLE coin_operations ADD CONSTRAINT coin_reversal_shape CHECK (reverses_operation_id IS NULL OR (kind = 'debit' AND source_type = 'manual_reversal' AND reverses_operation_id <> id));
CREATE UNIQUE INDEX coin_reversal_once ON coin_operations(reverses_operation_id) WHERE reverses_operation_id IS NOT NULL;
-- A stable external event cannot be issued again by changing operator or credit category.
CREATE UNIQUE INDEX coin_manual_event_once ON coin_operations(source_id) WHERE source_type IN ('manual_purchase', 'manual_reward');
