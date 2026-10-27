ALTER TABLE speech.preview_cache DROP CONSTRAINT speech_preview_mime_type;
ALTER TABLE speech.preview_cache ADD CONSTRAINT speech_preview_mime_type
    CHECK (mime_type IN ('audio/mpeg', 'audio/wav'));
