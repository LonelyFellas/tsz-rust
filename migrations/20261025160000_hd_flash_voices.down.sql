DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM speech.voices WHERE provider_voice_id LIKE '%:%')
       OR EXISTS (SELECT 1 FROM lexicon.audio_generations WHERE voice->>'voice_id' LIKE 'azure-hd-flash-%')
       OR EXISTS (SELECT 1 FROM speech.dubbing_groups WHERE groups::text LIKE '%azure-hd-flash-%')
       OR EXISTS (SELECT 1 FROM lexicon.entry_editor_projection WHERE forms::text LIKE '%azure-hd-flash-%' OR meanings::text LIKE '%azure-hd-flash-%')
       OR EXISTS (SELECT 1 FROM lexicon.v3_form_contents WHERE content::text LIKE '%azure-hd-flash-%')
       OR EXISTS (SELECT 1 FROM lexicon.entry_publications WHERE snapshot::text LIKE '%azure-hd-flash-%')
       OR EXISTS (SELECT 1 FROM lexicon.shared_sentences WHERE content::text LIKE '%azure-hd-flash-%')
       OR EXISTS (SELECT 1 FROM lexicon.shared_sentence_publications WHERE snapshot::text LIKE '%azure-hd-flash-%')
    THEN
        RAISE EXCEPTION 'Cannot roll back HD Flash support while model IDs or references exist';
    END IF;
END $$;
ALTER TABLE speech.voices DROP CONSTRAINT speech_voices_provider_voice_id_format;
ALTER TABLE speech.voices ADD CONSTRAINT speech_voices_provider_voice_id_format
    CHECK (provider_voice_id ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$');
