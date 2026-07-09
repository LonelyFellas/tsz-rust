DROP TABLE IF EXISTS student_profiles;   -- 触发器随表删
-- 注意：不要在这里 DROP set_updated_at()——它归 users 迁移所有，还被 users 表用着