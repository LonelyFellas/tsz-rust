-- OTP 域已迁至 Redis（见 docs/redis-design.md §7）：验证码的存储/单次消费/限流全部在 Redis
-- 上（TTL 自动过期、Lua 原子消费、ZSet 滚动日限），此表不再有任何读写方，删除。
-- DROP TABLE 会连带删掉 verification_codes_lookup 索引。
DROP TABLE IF EXISTS verification_codes;
