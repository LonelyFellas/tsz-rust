DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM speech.dubbing_audio_updates)
       OR EXISTS (SELECT 1 FROM lexicon.audio_generations WHERE effect_hash IS NOT NULL)
       OR EXISTS (SELECT 1 FROM speech.dubbing_update_settings WHERE enabled OR revision <> 0) THEN
        RAISE EXCEPTION 'dubbing audio update data exists; refuse destructive downgrade';
    END IF;
END $$;
DROP TABLE speech.dubbing_audio_update_targets;
DROP TABLE speech.dubbing_audio_updates;
DROP TABLE speech.dubbing_update_settings;
ALTER TABLE lexicon.audio_generations DROP COLUMN effect_hash;
