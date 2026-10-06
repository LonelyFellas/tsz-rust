LOCK TABLE coin_operations IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM coin_operations WHERE source_type IN ('manual_purchase', 'manual_reward', 'manual_reversal')) THEN
        RAISE EXCEPTION 'manual coin records must be retained';
    END IF;
END $$;
DROP INDEX coin_manual_event_once;
DROP INDEX coin_reversal_once;
ALTER TABLE coin_operations DROP CONSTRAINT coin_reversal_shape;
ALTER TABLE coin_operations DROP COLUMN reverses_operation_id;
