ALTER TABLE speech.voices DROP CONSTRAINT speech_voices_provider_voice_id_format;
ALTER TABLE speech.voices ADD CONSTRAINT speech_voices_provider_voice_id_format CHECK (
    provider_voice_id ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$'
    OR (provider = 'azure' AND provider_voice_id ~ '^en-[A-Z]{2}-[A-Za-z0-9][A-Za-z0-9_-]{0,95}:DragonHDFlashLatestNeural$'
        AND length(provider_voice_id) <= 128)
);
