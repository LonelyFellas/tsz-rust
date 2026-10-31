DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM speech.dubbing_audio_updates WHERE baseline_groups <> '{}'::jsonb)
       OR EXISTS (SELECT 1 FROM speech.dubbing_audio_update_targets WHERE applied IS NOT NULL) THEN
        RAISE EXCEPTION 'Cannot discard saved dubbing update baselines; retain this schema';
    END IF;
END $$;
ALTER TABLE speech.dubbing_audio_update_targets DROP COLUMN applied;
ALTER TABLE speech.dubbing_audio_updates DROP COLUMN baseline_groups;
