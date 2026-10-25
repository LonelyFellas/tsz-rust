CREATE TABLE lexicon.audio_generations (
    asset_id UUID PRIMARY KEY REFERENCES lexicon.audio_assets(id) ON DELETE CASCADE,
    request_id UUID NOT NULL,
    created_by_admin_id UUID NOT NULL REFERENCES admins(id),
    input_hash BYTEA NOT NULL CHECK (octet_length(input_hash) = 32),
    source JSONB NOT NULL,
    voice JSONB NOT NULL,
    resolved_content JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'ready', 'failed')),
    UNIQUE (created_by_admin_id, request_id)
);
