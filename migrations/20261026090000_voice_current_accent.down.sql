DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM speech.voices WHERE provider_voice_id LIKE '%:%' AND locale !~ '^en-[A-Z]{2}$')
       OR EXISTS (SELECT 1 FROM speech.voices WHERE accent <> CASE locale
        WHEN 'en-GB' THEN 'gb' WHEN 'en-AU' THEN 'gb' WHEN 'en-NZ' THEN 'gb'
        WHEN 'en-US' THEN 'us' WHEN 'en-CA' THEN 'us' ELSE 'none' END)
       OR EXISTS (SELECT 1 FROM speech.voice_descriptions WHERE description_zh IS NULL)
    THEN
        RAISE EXCEPTION 'Cannot roll back current voice accents or restored description revisions without losing saved data';
    END IF;
END $$;
ALTER TABLE speech.voices DROP CONSTRAINT speech_voices_provider_voice_id_format;
ALTER TABLE speech.voices ADD CONSTRAINT speech_voices_provider_voice_id_format CHECK (
    provider_voice_id ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$'
    OR (provider = 'azure' AND provider_voice_id ~ '^en-[A-Z]{2}-[A-Za-z0-9][A-Za-z0-9_-]{0,95}:DragonHDFlashLatestNeural$'
        AND length(provider_voice_id) <= 128)
);
ALTER TABLE speech.voice_descriptions ALTER COLUMN description_zh SET NOT NULL;
DROP TRIGGER initialize_voice_accent ON speech.voices;
DROP FUNCTION speech.initialize_voice_accent();
ALTER TABLE speech.voices DROP COLUMN accent, DROP COLUMN accent_revision;
