DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM speech.dubbing_audio_updates WHERE baseline_groups <> '{}'::jsonb) THEN
        RAISE EXCEPTION 'Cannot discard saved dubbing update baselines; retain this schema';
    END IF;
END $$;
ALTER TABLE speech.dubbing_audio_updates DROP COLUMN baseline_groups;
