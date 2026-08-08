-- 支撑后台用户列表的注册时间范围过滤与稳定倒序分页。
CREATE INDEX users_created_at_id_idx
    ON users (created_at DESC, id DESC);
