CREATE TABLE speech.voice_descriptions (
    voice_id UUID PRIMARY KEY REFERENCES speech.voices(id) ON DELETE CASCADE,
    description_zh TEXT NOT NULL CHECK (char_length(description_zh) <= 3000),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    updated_by UUID NOT NULL REFERENCES admins(id),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE speech.admin_voice_settings (
    admin_id UUID NOT NULL REFERENCES admins(id) ON DELETE CASCADE,
    voice_id UUID NOT NULL REFERENCES speech.voices(id) ON DELETE CASCADE,
    parameters JSONB CHECK (parameters IS NULL OR jsonb_typeof(parameters) = 'object'),
    notes TEXT NOT NULL DEFAULT '' CHECK (char_length(notes) <= 3000),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (admin_id, voice_id)
);
