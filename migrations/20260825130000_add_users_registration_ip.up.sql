-- 注册来源 IP：注册那一刻从反代 X-Forwarded-For 取到的客户端地址，只写一次、之后不再更新。
-- 用途是后台按地区看用户分布（IP → 省市的解析放在读取侧，这里只忠实保留原始地址）。
-- 可空：反代未配置 XFF、或历史行，都合法留 NULL。
ALTER TABLE users ADD COLUMN registration_ip TEXT;
