ALTER TABLE admins
    ADD COLUMN created_by_admin_id UUID NULL;

ALTER TABLE admins
    ADD CONSTRAINT admins_created_by_admin_id_fkey
    FOREIGN KEY (created_by_admin_id)
    REFERENCES admins(id)
    ON DELETE RESTRICT;

CREATE INDEX admins_created_by_admin_id_idx
    ON admins(created_by_admin_id);

COMMENT ON COLUMN admins.created_by_admin_id
    IS '创建该管理员的超级管理员；NULL 表示 seed 创建或历史数据';
