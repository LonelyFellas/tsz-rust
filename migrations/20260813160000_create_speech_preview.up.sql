CREATE SCHEMA IF NOT EXISTS speech;

CREATE TABLE speech.voices (
    id UUID PRIMARY KEY,
    alias TEXT NOT NULL UNIQUE,
    provider TEXT NOT NULL,
    provider_voice_id TEXT NOT NULL,
    locale TEXT NOT NULL,
    gender TEXT NOT NULL,
    styles JSONB NOT NULL DEFAULT '[]'::jsonb,
    min_rate_percent SMALLINT NOT NULL DEFAULT -50,
    max_rate_percent SMALLINT NOT NULL DEFAULT 100,
    min_pitch_semitones SMALLINT NOT NULL DEFAULT -12,
    max_pitch_semitones SMALLINT NOT NULL DEFAULT 12,
    provider_version TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT speech_voices_alias_format CHECK (alias ~ '^[a-z0-9][a-z0-9_-]{0,63}$'),
    CONSTRAINT speech_voices_provider_format CHECK (provider ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$'),
    CONSTRAINT speech_voices_provider_voice_id_format CHECK (provider_voice_id ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$'),
    CONSTRAINT speech_voices_locale_format CHECK (locale ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,34}$'),
    CONSTRAINT speech_voices_gender CHECK (gender IN ('female', 'male', 'neutral')),
    CONSTRAINT speech_voices_styles_array CHECK (jsonb_typeof(styles) = 'array'),
    CONSTRAINT speech_voices_rate_range CHECK (min_rate_percent >= -50 AND max_rate_percent <= 100 AND min_rate_percent <= max_rate_percent),
    CONSTRAINT speech_voices_pitch_range CHECK (min_pitch_semitones >= -12 AND max_pitch_semitones <= 12 AND min_pitch_semitones <= max_pitch_semitones),
    CONSTRAINT speech_voices_provider_version_nonempty CHECK (provider_version <> '')
);

CREATE INDEX speech_voices_enabled_alias_idx ON speech.voices (alias) WHERE enabled;

CREATE TABLE speech.preview_cache (
    request_hash BYTEA PRIMARY KEY,
    voice_id UUID NOT NULL REFERENCES speech.voices(id) ON DELETE RESTRICT,
    content_hash BYTEA NOT NULL,
    object_key TEXT NOT NULL UNIQUE,
    mime_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT speech_preview_request_hash_length CHECK (octet_length(request_hash) = 32),
    CONSTRAINT speech_preview_content_hash_length CHECK (octet_length(content_hash) = 32),
    CONSTRAINT speech_preview_object_key_nonempty CHECK (object_key <> ''),
    CONSTRAINT speech_preview_mime_type CHECK (mime_type = 'audio/mpeg'),
    CONSTRAINT speech_preview_size_positive CHECK (size_bytes > 0),
    CONSTRAINT speech_preview_expiry CHECK (expires_at > created_at)
);

CREATE INDEX speech_preview_cache_expiry_idx ON speech.preview_cache (expires_at);
