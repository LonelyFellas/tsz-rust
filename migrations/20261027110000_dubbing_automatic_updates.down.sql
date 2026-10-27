-- Removing the execution switch loses its previous value; do not invent it on rollback.
-- Keep the migrated database intact for an explicitly approved manual recovery plan.
DO $$ BEGIN
    RAISE EXCEPTION 'automatic dubbing updates require manual recovery; automatic downgrade is not supported';
END $$;
