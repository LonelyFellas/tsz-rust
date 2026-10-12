ALTER TABLE refresh_tokens
    ADD COLUMN rotation_request_hash TEXT,
    ADD COLUMN replacement_token_hash TEXT;
ALTER TABLE admin_refresh_tokens
    ADD COLUMN rotation_request_hash TEXT,
    ADD COLUMN replacement_token_hash TEXT;
CREATE UNIQUE INDEX refresh_tokens_replacement ON refresh_tokens (replacement_token_hash)
    WHERE replacement_token_hash IS NOT NULL;
CREATE UNIQUE INDEX admin_refresh_tokens_replacement ON admin_refresh_tokens (replacement_token_hash)
    WHERE replacement_token_hash IS NOT NULL;
