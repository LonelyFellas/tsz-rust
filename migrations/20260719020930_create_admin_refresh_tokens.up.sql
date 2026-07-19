CREATE TABLE admin_refresh_tokens (
    id          UUID        PRIMARY KEY,
    admin_id    UUID        NOT NULL REFERENCES admins(id) ON DELETE CASCADE,
    token_hash  TEXT        NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    revoked_at  TIMESTAMPTZ,
    rotated_at  TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX admin_refresh_tokens_admin ON admin_refresh_tokens (admin_id);
CREATE UNIQUE INDEX admin_refresh_tokens_hash ON admin_refresh_tokens (token_hash);