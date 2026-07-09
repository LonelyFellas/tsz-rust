-- 学生资料。每个用户最多一份（user_id 作主键，是 users 的 1:1 扩展）。
-- 承载学习设置，这两项驱动学习内容的下发。
CREATE TABLE student_profiles (
    user_id         UUID        PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    grade           TEXT        NOT NULL DEFAULT '',

    -- 学习设置（onboarding 两选择）。两者可空；「是否 onboarded」由它们是否已设推导。
    cefr_level      TEXT        CHECK (cefr_level IN ('A1','A2','B1','B2','C1','C2')),
    english_variant TEXT        CHECK (english_variant IN ('BrE','AmE')),

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- 全有或全无：两个学习设置成对写入，杜绝「半设置」。
    -- (A IS NULL) = (B IS NULL)：都空→放行，都设→放行，只设一个→拒绝。
    CONSTRAINT student_profiles_learning_settings_paired
        CHECK ((cefr_level IS NULL) = (english_variant IS NULL))
);

-- 复用 users 迁移里已定义的 set_updated_at()，UPDATE 时自动刷新 updated_at。
CREATE TRIGGER student_profiles_set_updated_at
    BEFORE UPDATE ON student_profiles
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();