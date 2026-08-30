-- 正式回滚只关闭 Pending 写入口并保留 additive schema/数据。自动 down 无法区分
-- “尚未认领但必须保留”的 Pending，因此宁可显式失败，也不能静默删除业务数据。
DO $$
BEGIN
    RAISE EXCEPTION 'pending sentence associations migration is non-destructive and cannot be reverted automatically'
        USING ERRCODE = '0A000';
END
$$;
