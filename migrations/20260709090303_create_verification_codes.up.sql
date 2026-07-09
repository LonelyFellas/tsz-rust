-- 一次性验证码（OTP）。target = 目标手机号/邮箱，channel = 下发渠道，purpose = 用途。
-- 故意不建外键指向 users：注册前就要发码，target 是裸标识、可能还没对应账号。
CREATE TABLE verification_codes (
    id          UUID        PRIMARY KEY,
    target      TEXT        NOT NULL,
    channel     TEXT        NOT NULL CHECK (channel IN ('sms', 'email')),
    purpose     TEXT        NOT NULL CHECK (purpose IN ('login', 'password_reset', 'account_deletion', 'contact_bind')),
    code        TEXT        NOT NULL,               -- 6 位数字
    expires_at  TIMESTAMPTZ NOT NULL,               -- 时限
    consumed_at TIMESTAMPTZ,                         -- 单次使用标记，NULL = 未用
    attempts    INT         NOT NULL DEFAULT 0,      -- 失败次数，到上限锁死（防爆破）
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Verify 查「某 target+purpose 最近一条未消费的码」。
CREATE INDEX verification_codes_lookup ON verification_codes (target, purpose, created_at DESC);