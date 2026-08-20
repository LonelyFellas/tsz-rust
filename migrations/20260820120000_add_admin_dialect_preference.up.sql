-- 管理员的英语方言偏好（英美方言偏好化 A1 · 后端提案 P2）。
-- 账号级个人设置，只决定 admin 端的录入与展示口径，不是词条属性：
-- 一条词条同时有英式 / 美式两种拼写是词典事实，由 lexicon.entry_headwords 承载，
-- 不因某个管理员偏好英式就消失。
-- 用列而不是 preferences jsonb：两态开关的取值约束交给数据库，
-- 与同表的 role / status 一致；将来加第二个偏好再加一列。
ALTER TABLE admins
    ADD COLUMN dialect_preference TEXT NOT NULL DEFAULT 'uk'
        CONSTRAINT admins_dialect_preference_check
        CHECK (dialect_preference IN ('uk', 'us'));

COMMENT ON COLUMN admins.dialect_preference
    IS '管理员英语方言偏好；默认英式，存量账号一并按英式解释';
