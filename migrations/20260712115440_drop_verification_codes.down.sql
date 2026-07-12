-- 回滚：原样重建 verification_codes 表与索引（与 20260709090303_create_verification_codes 一致）。
CREATE TABLE verification_codes (
    id          UUID        PRIMARY KEY,
    target      TEXT        NOT NULL,
    channel     TEXT        NOT NULL CHECK (channel IN ('sms', 'email')),
    purpose     TEXT        NOT NULL CHECK (purpose IN ('login', 'password_reset', 'account_deletion', 'contact_bind')),
    code        TEXT        NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    attempts    INT         NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX verification_codes_lookup ON verification_codes (target, purpose, created_at DESC);
