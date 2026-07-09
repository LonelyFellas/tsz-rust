CREATE TABLE users (
    id UUID PRIMARY KEY,
    -- 标识，手机，邮箱，二选一（至少一个）。
    phone TEXT, 
    email TEXT,
    password_hash TEXT NOT NULL,
    display_name TEXT NOT NULL,
    -- 账号生命周期。默认active;disabled 在登录边界拦截
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),

    -- 当前激活角色，持久化以熬过 token 刷新， NULL = 从未切换 -> 回落默认角色
    last_active_role TEXT CHECK (last_active_role IN ('student', 'teacher')),
    -- 头像的不透明引用（文件在 OSS）。默认 '' 让每行无头像也合法。
    avatar_url TEXT NOT NULL DEFAULT '',
    -- 创建时间/更新时间
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- 每个账号至少有一个可达标识，数据库边界即拒绝两者皆空
    CONSTRAINT users_phone_or_email_present CHECK (phone IS NOT NULL OR email IS NOT NULL)
);

-- 手机唯一，但只对有手机的行（部分唯一，配合二选一）。
CREATE UNIQUE INDEX users_phone_key ON users (phone) WHERE phone IS NOT NULL;

-- 邮箱大小写不敏感唯一，且只对有邮箱的行。应用写入前也应先小写化。
CREATE UNIQUE INDEX users_email_key ON users (lower(email)) WHERE email IS NOT NULL;

CREATE OR REPLACE FUNCTION set_updated_at() RETURNS trigger AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER users_set_updated_at
    BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();