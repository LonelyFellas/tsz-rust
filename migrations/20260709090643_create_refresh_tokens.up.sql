-- 会话。access token 是无状态 JWT（中间件本地校验）；refresh token 是不透明高熵
-- 随机串，这里只存它的 SHA-256 哈希。只有低频的 /auth/refresh、/auth/logout 碰这表。
CREATE TABLE refresh_tokens (
    id          UUID        PRIMARY KEY,
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash  TEXT        NOT NULL,              -- 存哈希，不存明文
    expires_at  TIMESTAMPTZ NOT NULL,
    revoked_at  TIMESTAMPTZ,                        -- 硬吊销（登出/异地登录/改密），无宽限
    rotated_at  TIMESTAMPTZ,                        -- 轮换消费；宽限窗口逻辑在 service 层（先留列）
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX refresh_tokens_user ON refresh_tokens (user_id);
CREATE UNIQUE INDEX refresh_tokens_hash ON refresh_tokens (token_hash);   -- 哈希唯一