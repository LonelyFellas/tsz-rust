use deadpool_redis::{Pool, redis::Script};
use std::time::Duration;

use crate::otp::model::Purpose;

#[derive(Debug, thiserror::Error)]
pub enum OtpStoreError {
    #[error(transparent)]
    RedisError(#[from] deadpool_redis::PoolError),
    #[error(transparent)]
    Command(#[from] deadpool_redis::redis::RedisError),
}

pub struct OtpStore {
    redis: Pool,
    prefix: String,
}

impl OtpStore {
    pub fn new(redis: Pool) -> Self {
        Self {
            redis,
            prefix: String::new(),
        }
    }

    pub fn with_prefix(redis: Pool, prefix: String) -> Self {
        Self { redis, prefix }
    }

    pub async fn save_code(
        &self,
        target: &str,
        purpose: Purpose,
        code: &str,
        ttl: Duration,
    ) -> Result<(), OtpStoreError> {
        let key = self.code_key(target, purpose);
        let mut conn = self.redis.get().await?; // 从池取连接

        // HSET + EXPIRE 用atomic pipeline (MULTI?EXEC) 一起提交
        // 否则「HSET 成功、EXPIRE 没发出」就留下一个永久键（码永不过期 + 内存泄漏）
        deadpool_redis::redis::pipe()
            .atomic()
            .cmd("HSET")
            .arg(&key)
            .arg("code")
            .arg(code)
            .arg("attempts")
            .arg(0i64)
            .ignore()
            .cmd("EXPIRE")
            .arg(&key)
            .arg(ttl.as_secs() as i64)
            .ignore()
            .query_async::<()>(&mut conn)
            .await?;

        Ok(())
    }

    pub async fn verify_code(
        &self,
        target: &str,
        purpose: Purpose,
        submitted: &str,
        max_attempts: u8,
    ) -> Result<bool, OtpStoreError> {
        let key = self.code_key(target, purpose);
        let mut conn = self.redis.get().await?;

        let script = Script::new(
            r#"
            local stored = redis.call('HGET', KEYS[1], 'code')
            if not stored then return 0 end
            if stored == ARGV[1] then
                redis.call('DEL', KEYS[1])
                return 1
            end
            local n = redis.call('HINCRBY', KEYS[1], 'attempts', 1)
            if n >= tonumber(ARGV[2]) then
                redis.call('DEL', KEYS[1])
            end
            return 0
            "#,
        );

        let outcome: i64 = script
            .key(&key) // → KEYS[1]
            .arg(submitted) // → ARGV[1]
            .arg(max_attempts) // → ARGV[2]（u8 会以字符串 "3" 传入，Lua 里 tonumber 还原）
            .invoke_async(&mut conn)
            .await?;

        Ok(outcome == 1)
    }

    /// 【测试接缝】只读探测 code 键是否存在——不校验、不消费、不动 attempts。
    /// 供「恒 202 压平」类测试断言：静默分支确实没落码 / 真发分支确实落了
    /// （`OtpSender::Mock` 不回传码，生成的码测试拿不到，只能验键存在性）。
    /// 生产代码不得调用：判码一律走 `verify_code`（CAS 单次消费）。
    pub async fn code_exists(&self, target: &str, purpose: Purpose) -> Result<bool, OtpStoreError> {
        let key = self.code_key(target, purpose);
        let mut conn = self.redis.get().await?;
        let n: i64 = deadpool_redis::redis::cmd("EXISTS")
            .arg(&key)
            .query_async(&mut conn)
            .await?;
        Ok(n == 1)
    }

    pub async fn check_cooldown(
        &self,
        target: &str,
        purpose: Purpose,
        cooldown: Duration,
    ) -> Result<bool, OtpStoreError> {
        let key = self.cd_key(target, purpose); // ← 记得带 self.prefix
        let mut conn = self.redis.get().await?;

        // SET key 1 NX EX <秒>：成功 → 返回 "OK"(Some)；键已存在 → 返回 nil(None)。
        let set: Option<String> = deadpool_redis::redis::cmd("SET")
            .arg(&key)
            .arg(1)
            .arg("NX")
            .arg("EX")
            .arg(cooldown.as_secs() as i64)
            .query_async(&mut conn)
            .await?;

        Ok(set.is_some()) // Some = 放行（刚占位）；None = 冷却中
    }

    pub async fn check_daily_limit(
        &self,
        target: &str,
        purpose: Purpose,
        limit: u64,
        window: Duration,
    ) -> Result<bool, OtpStoreError> {
        let key = self.daily_key(target, purpose); // ← 记得带 self.prefix
        let member = uuid::Uuid::now_v7().to_string(); // 唯一 member，防同刻覆盖少计
        let mut conn = self.redis.get().await?;

        let script = Script::new(
            r#"
            -- KEYS[1]=日限键  ARGV[1]=limit  ARGV[2]=window秒  ARGV[3]=member(uuid)
            local now = tonumber(redis.call('TIME')[1])          -- Redis 服务器时钟，秒
            local cutoff = now - tonumber(ARGV[2])
            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', cutoff)   -- 修剪窗外旧事件
            local n = redis.call('ZCARD', KEYS[1])
            if n >= tonumber(ARGV[1]) then
                return 0                                          -- 达上限：不追加
            end
            redis.call('ZADD', KEYS[1], now, ARGV[3])            -- 记录本次
            redis.call('EXPIRE', KEYS[1], ARGV[2])               -- 自清理，防内存泄漏
            return 1
            "#,
        );

        let outcome: i64 = script
            .key(&key)
            .arg(limit)
            .arg(window.as_secs() as i64)
            .arg(member)
            .invoke_async(&mut conn)
            .await?;

        Ok(outcome == 1)
    }

    fn code_key(&self, target: &str, purpose: Purpose) -> String {
        format!(
            "{}otp:code:{}:{}",
            self.prefix,
            target,
            purpose.as_key_segment()
        )
    }

    fn cd_key(&self, target: &str, purpose: Purpose) -> String {
        format!(
            "{}otp:cd:{}:{}",
            self.prefix,
            target,
            purpose.as_key_segment()
        )
    }

    fn daily_key(&self, target: &str, purpose: Purpose) -> String {
        format!(
            "{}otp:daily:{}:{}",
            self.prefix,
            target,
            purpose.as_key_segment()
        )
    }
}
