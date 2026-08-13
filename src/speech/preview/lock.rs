use std::time::Duration;

use deadpool_redis::{Pool, PoolError, redis};
use thiserror::Error;
use uuid::Uuid;

pub struct PreviewLock {
    pool: Pool,
    key: String,
    token: String,
}

#[derive(Debug, Error)]
pub enum LockError {
    #[error("redis pool unavailable")]
    Pool(#[from] PoolError),
    #[error("redis command failed")]
    Redis(#[from] redis::RedisError),
}

impl PreviewLock {
    pub async fn acquire(
        pool: Pool,
        hash: &[u8],
        ttl: Duration,
    ) -> Result<Option<Self>, LockError> {
        let key = format!("speech:preview:lock:{}", encode_hex(hash));
        let token = Uuid::now_v7().to_string();
        let mut connection = pool.get().await?;
        let result: Option<String> = redis::cmd("SET")
            .arg(&key)
            .arg(&token)
            .arg("NX")
            .arg("PX")
            .arg(u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX))
            .query_async(&mut connection)
            .await?;
        Ok(result.map(|_| Self { pool, key, token }))
    }

    pub async fn release(self) -> Result<(), LockError> {
        let script = redis::Script::new(
            "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) else return 0 end",
        );
        let mut connection = self.pool.get().await?;
        let _: i64 = script
            .key(self.key)
            .arg(self.token)
            .invoke_async(&mut connection)
            .await?;
        Ok(())
    }
}

pub async fn wait_interval() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> Pool {
        deadpool_redis::Config::from_url("redis://127.0.0.1:6379/0")
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .unwrap()
    }

    #[tokio::test]
    async fn lease_expiry_recovers_and_stale_owner_cannot_release_new_owner() {
        let hash = uuid::Uuid::now_v7().as_bytes().to_owned();
        let stale = PreviewLock::acquire(pool(), &hash, Duration::from_millis(50))
            .await
            .unwrap()
            .expect("first owner acquires");
        assert!(
            PreviewLock::acquire(pool(), &hash, Duration::from_secs(1))
                .await
                .unwrap()
                .is_none()
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
        let current = PreviewLock::acquire(pool(), &hash, Duration::from_secs(1))
            .await
            .unwrap()
            .expect("expired lease is recoverable");
        stale.release().await.unwrap();
        assert!(
            PreviewLock::acquire(pool(), &hash, Duration::from_secs(1))
                .await
                .unwrap()
                .is_none(),
            "stale token must not delete current owner"
        );
        current.release().await.unwrap();
    }
}
