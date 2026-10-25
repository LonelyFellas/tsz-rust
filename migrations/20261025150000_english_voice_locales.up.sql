ALTER TABLE lexicon.audio_assets DROP CONSTRAINT lexicon_audio_assets_locale;
ALTER TABLE lexicon.audio_assets ADD CONSTRAINT lexicon_audio_assets_locale
    CHECK (locale ~ '^en-[A-Z]{2}$');
ALTER TABLE lexicon.audio_assets DROP CONSTRAINT lexicon_audio_assets_gender;
ALTER TABLE lexicon.audio_assets ADD CONSTRAINT lexicon_audio_assets_gender
    CHECK (gender IN ('female', 'male', 'neutral'));
