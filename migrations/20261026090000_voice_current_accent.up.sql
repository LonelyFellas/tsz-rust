ALTER TABLE speech.voices DROP CONSTRAINT speech_voices_provider_voice_id_format;
ALTER TABLE speech.voices ADD CONSTRAINT speech_voices_provider_voice_id_format CHECK (
    provider_voice_id ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$'
    OR (provider = 'azure' AND provider_voice_id ~ '^[A-Za-z0-9-]+:DragonHDFlashLatestNeural$'
        AND length(provider_voice_id) <= 128)
);

ALTER TABLE speech.voices
    ADD COLUMN accent TEXT,
    ADD COLUMN accent_revision BIGINT NOT NULL DEFAULT 0 CHECK (accent_revision >= 0);

UPDATE speech.voices SET accent = CASE locale
    WHEN 'en-GB' THEN 'gb' WHEN 'en-AU' THEN 'gb' WHEN 'en-NZ' THEN 'gb'
    WHEN 'en-US' THEN 'us' WHEN 'en-CA' THEN 'us'
    ELSE 'none' END;

ALTER TABLE speech.voices
    ALTER COLUMN accent SET NOT NULL,
    ADD CONSTRAINT speech_voices_accent CHECK (accent IN ('gb', 'us', 'none'));

CREATE FUNCTION speech.initialize_voice_accent() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.accent IS NULL THEN
        NEW.accent := CASE NEW.locale
            WHEN 'en-GB' THEN 'gb' WHEN 'en-AU' THEN 'gb' WHEN 'en-NZ' THEN 'gb'
            WHEN 'en-US' THEN 'us' WHEN 'en-CA' THEN 'us'
            ELSE 'none' END;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER initialize_voice_accent BEFORE INSERT ON speech.voices
    FOR EACH ROW EXECUTE FUNCTION speech.initialize_voice_accent();

-- Keep a revision-bearing row after restoring the official description.
ALTER TABLE speech.voice_descriptions ALTER COLUMN description_zh DROP NOT NULL;
