-- 教师资料。每个用户最多一份（user_id 作主键，是 users 的 1:1 扩展）。
-- 无学习设置，所以没有 student_profiles 那条成对 CHECK。
CREATE TABLE teacher_profiles (
    user_id    UUID        PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    bio        TEXT        NOT NULL DEFAULT '',
    verified   BOOLEAN     NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 复用 users 迁移里的 set_updated_at()。
CREATE TRIGGER teacher_profiles_set_updated_at
    BEFORE UPDATE ON teacher_profiles
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();