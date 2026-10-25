DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM lexicon.audio_assets WHERE locale NOT IN ('en-GB', 'en-US') OR gender = 'neutral')
       OR EXISTS (SELECT 1 FROM lexicon.audio_generations WHERE
           jsonb_path_exists(voice, '$.**.locale ? (@ != "en-GB" && @ != "en-US" && @ like_regex "^en-[A-Z]{2}$")'))
       OR EXISTS (SELECT 1 FROM speech.voices WHERE locale ~ '^en-[A-Z]{2}$' AND (locale NOT IN ('en-GB', 'en-US') OR gender = 'neutral'))
       OR EXISTS (SELECT 1 FROM lexicon.entry_editor_projection WHERE
           jsonb_path_exists(forms, '$.**.locale ? (@ != "en-GB" && @ != "en-US" && @ like_regex "^en-[A-Z]{2}$")')
           OR jsonb_path_exists(meanings, '$.**.locale ? (@ != "en-GB" && @ != "en-US" && @ like_regex "^en-[A-Z]{2}$")'))
       OR EXISTS (SELECT 1 FROM lexicon.v3_form_contents WHERE
           jsonb_path_exists(content, '$.**.locale ? (@ != "en-GB" && @ != "en-US" && @ like_regex "^en-[A-Z]{2}$")'))
       OR EXISTS (SELECT 1 FROM lexicon.entry_publications WHERE
           jsonb_path_exists(snapshot, '$.**.locale ? (@ != "en-GB" && @ != "en-US" && @ like_regex "^en-[A-Z]{2}$")'))
       OR EXISTS (SELECT 1 FROM lexicon.shared_sentences WHERE
           jsonb_path_exists(content, '$.**.locale ? (@ != "en-GB" && @ != "en-US" && @ like_regex "^en-[A-Z]{2}$")'))
       OR EXISTS (SELECT 1 FROM lexicon.shared_sentence_publications WHERE
           jsonb_path_exists(snapshot, '$.**.locale ? (@ != "en-GB" && @ != "en-US" && @ like_regex "^en-[A-Z]{2}$")'))
    THEN
        RAISE EXCEPTION 'Cannot roll back English voice locales while extended locale data exists';
    END IF;
END $$;
ALTER TABLE lexicon.audio_assets DROP CONSTRAINT lexicon_audio_assets_locale;
ALTER TABLE lexicon.audio_assets ADD CONSTRAINT lexicon_audio_assets_locale
    CHECK (locale IN ('en-GB', 'en-US'));
ALTER TABLE lexicon.audio_assets DROP CONSTRAINT lexicon_audio_assets_gender;
ALTER TABLE lexicon.audio_assets ADD CONSTRAINT lexicon_audio_assets_gender
    CHECK (gender IN ('female', 'male'));
