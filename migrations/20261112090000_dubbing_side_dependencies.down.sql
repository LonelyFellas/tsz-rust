DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM speech.dubbing_dependencies WHERE accents <> ARRAY['gb','us']::TEXT[]) THEN
        RAISE EXCEPTION 'Cannot discard independent accent dependencies';
    END IF;
END $$;
ALTER TABLE speech.dubbing_dependencies DROP COLUMN accents;
