CREATE TABLE admins (
    id UUID PRIMARY KEY,
    phone VARCHAR(255) NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'admin' CHECK (role IN ('admin', 'super_admin')),
    -- 初始化密码后，必须修改密码。
    must_change_password BOOLEAN NOT NULL DEFAULT TRUE,
    -- 登录失败次数，超过一定次数后锁定。
    failed_login_count INT NOT NULL DEFAULT 0,
    -- 锁定时间，超过锁定时间后解锁。
    -- 连续失败触发的自动防爆破,423
    locked_until TIMESTAMPTZ,
    -- 状态，active 正常，disabled 是禁用(超管的管理动作,403)
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);