DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM speech.preview_cache WHERE mime_type <> 'audio/mpeg')
       OR EXISTS (SELECT 1 FROM lexicon.audio_generations WHERE voice ? 'output_format')
       OR EXISTS (SELECT 1 FROM lexicon.entry_editor_projection WHERE jsonb_path_exists(jsonb_build_array(forms,meanings),'$.**.output_format'))
       OR EXISTS (SELECT 1 FROM lexicon.entry_editor_projection WHERE jsonb_path_exists(forms,'$.**.spelling_speech.audio_assets[*]'))
       OR EXISTS (SELECT 1 FROM lexicon.entry_publications WHERE jsonb_path_exists(snapshot,'$.**.output_format') OR jsonb_path_exists(snapshot,'$.**.spelling_speech.audio_assets[*]'))
       OR EXISTS (SELECT 1 FROM lexicon.shared_sentences WHERE jsonb_path_exists(content,'$.**.output_format'))
       OR EXISTS (SELECT 1 FROM lexicon.shared_sentence_publications WHERE jsonb_path_exists(snapshot,'$.**.output_format')) THEN
        RAISE EXCEPTION 'speech audio format data exists; refuse destructive downgrade';
    END IF;
END $$;
ALTER TABLE speech.preview_cache DROP CONSTRAINT speech_preview_mime_type;
ALTER TABLE speech.preview_cache ADD CONSTRAINT speech_preview_mime_type CHECK (mime_type = 'audio/mpeg');
