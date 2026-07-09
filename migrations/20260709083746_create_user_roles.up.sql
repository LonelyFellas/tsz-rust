-- 用户持有的角色。一个账号可同时是 student、teacher 或两者；
-- 当前激活角色在 JWT / users.last_active_role，这里只记「持有哪些」。
CREATE TABLE user_roles (
    user_id    UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    -- 角色值：TEXT + CHECK(student/teacher)（和 users.last_active_role 同一套值）
    role       TEXT        NOT NULL CHECK (role IN ('student', 'teacher')),

    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- 复合主键：同一用户同一角色不可重复；无需单独 id（组合即主键）。
    PRIMARY KEY (user_id, role)
);