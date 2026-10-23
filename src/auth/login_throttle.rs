//! 密码登录预算：IP 请求固定窗 + 账号失败/在途固定窗，均由 Redis 原子裁决。
use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;
use deadpool_redis::{Pool, redis::Script};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{AppError, ErrorCode};

const WINDOW_SECONDS: u64 = 15 * 60;
const IDENTIFIER_LIMIT: u64 = 10;
const IP_LIMIT: u64 = 100;

const IP_ATTEMPT: &str = r#"
local count = tonumber(redis.call('GET', KEYS[1]) or '0')
if count >= tonumber(ARGV[1]) then
    return math.max(1, redis.call('TTL', KEYS[1]))
end
count = redis.call('INCR', KEYS[1])
if count == 1 then redis.call('EXPIRE', KEYS[1], ARGV[2]) end
return 0
"#;

// 预约先占预算，不能让并发请求全部通过“失败数 < 上限”的检查再去验密码。
// 固定 TTL 不因拒绝或完成而续期；取消/超时的预约至多占用当前窗口。
const RESERVE: &str = r#"
local generation = redis.call('HGET', KEYS[1], 'generation')
if not generation then
    generation = ARGV[3]
    redis.call('HSET', KEYS[1], 'generation', generation, 'failed', 0, 'pending', 0)
    redis.call('EXPIRE', KEYS[1], ARGV[2])
end
local failed = tonumber(redis.call('HGET', KEYS[1], 'failed'))
local pending = tonumber(redis.call('HGET', KEYS[1], 'pending'))
if failed + pending >= tonumber(ARGV[1]) then
    return {math.max(1, redis.call('TTL', KEYS[1])), generation}
end
redis.call('HINCRBY', KEYS[1], 'pending', 1)
return {0, generation}
"#;

const FINISH: &str = r#"
-- 旧窗口的慢请求不能修改新窗口，也不能在过期后重新创建计数。
if redis.call('HGET', KEYS[1], 'generation') ~= ARGV[1] then return 0 end
local pending = redis.call('HINCRBY', KEYS[1], 'pending', -1)
if ARGV[2] == 'success' then
    redis.call('HSET', KEYS[1], 'failed', 0)
elseif ARGV[2] == 'failure' then
    redis.call('HINCRBY', KEYS[1], 'failed', 1)
end
if pending == 0 and tonumber(redis.call('HGET', KEYS[1], 'failed')) == 0 then
    redis.call('DEL', KEYS[1])
end
return 0
"#;

#[derive(Clone)]
pub(crate) struct LoginThrottle {
    pool: Pool,
    prefix: String,
}

pub(crate) struct Attempt {
    key: String,
    generation: String,
}

pub(crate) enum Outcome {
    Success,
    Failure,
    Unavailable,
}

impl LoginThrottle {
    pub(crate) fn new(pool: Pool, prefix: String) -> Self {
        Self { pool, prefix }
    }

    fn key(&self, dimension: &str, value: &str) -> String {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(value));
        format!("{}{dimension}:{digest}", self.prefix)
    }

    pub(crate) async fn check_ip(&self, ip: &str) -> Result<(), AppError> {
        let mut conn = self.pool.get().await.map_err(unavailable)?;
        let retry: u64 = Script::new(IP_ATTEMPT)
            .key(self.key("ip", ip))
            .arg(IP_LIMIT)
            .arg(WINDOW_SECONDS)
            .invoke_async(&mut conn)
            .await
            .map_err(unavailable)?;
        check_retry(retry)
    }

    pub(crate) async fn reserve(&self, scope: &str) -> Result<Attempt, AppError> {
        let key = self.key("account", scope);
        let mut conn = self.pool.get().await.map_err(unavailable)?;
        let (retry, generation): (u64, String) = Script::new(RESERVE)
            .key(&key)
            .arg(IDENTIFIER_LIMIT)
            .arg(WINDOW_SECONDS)
            .arg(Uuid::new_v4().to_string())
            .invoke_async(&mut conn)
            .await
            .map_err(unavailable)?;
        check_retry(retry)?;
        Ok(Attempt { key, generation })
    }

    pub(crate) async fn finish(&self, attempt: Attempt, outcome: Outcome) -> Result<(), AppError> {
        let mut conn = self.pool.get().await.map_err(unavailable)?;
        Script::new(FINISH)
            .key(attempt.key)
            .arg(attempt.generation)
            .arg(match outcome {
                Outcome::Success => "success",
                Outcome::Failure => "failure",
                Outcome::Unavailable => "unavailable",
            })
            .invoke_async::<i64>(&mut conn)
            .await
            .map_err(unavailable)?;
        Ok(())
    }
}

fn check_retry(retry: u64) -> Result<(), AppError> {
    if retry > 0 {
        Err(
            AppError::rate_limited(ErrorCode::LoginRateLimited, "too many login attempts")
                .with_retry_after(retry),
        )
    } else {
        Ok(())
    }
}

fn unavailable(error: impl Into<anyhow::Error>) -> AppError {
    AppError::unavailable_with_source(
        ErrorCode::LoginUnavailable,
        "login temporarily unavailable",
        error,
    )
}

/// 同机反代是显式信任边界：nginx 必须覆盖 X-Real-IP。外部 TCP peer 的头全部忽略。
/// 缺少连接信息（例如直接调用 Router）共享 unknown 桶，不能用伪造头绕过预算。
pub(crate) fn source_ip(peer: Option<SocketAddr>, headers: &HeaderMap) -> String {
    let Some(peer) = peer else {
        return "unknown".to_owned();
    };
    let peer = canonical_ip(peer.ip());
    if peer.is_loopback() {
        let candidate = match headers.get("x-real-ip") {
            Some(value) => value.to_str().ok(),
            None => headers
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.rsplit(',').next()),
        };
        if let Some(ip) = candidate.and_then(|value| value.trim().parse::<IpAddr>().ok()) {
            return canonical_ip(ip).to_string();
        }
    }
    peer.to_string()
}

fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        ip => ip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deadpool_redis::redis::{AsyncCommands, cmd};

    fn peer(value: &str) -> Option<SocketAddr> {
        Some(value.parse().unwrap())
    }

    #[test]
    fn untrusted_peers_and_missing_connect_info_ignore_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "203.0.113.5".parse().unwrap());
        headers.insert("x-forwarded-for", "203.0.113.6".parse().unwrap());
        assert_eq!(
            source_ip(peer("198.51.100.7:4321"), &headers),
            "198.51.100.7"
        );
        assert_eq!(
            source_ip(peer("[::ffff:198.51.100.7]:4321"), &headers),
            "198.51.100.7"
        );
        assert_eq!(source_ip(None, &headers), "unknown");
    }

    #[test]
    fn loopback_proxy_uses_canonical_real_ip_then_rightmost_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "::ffff:203.0.113.5".parse().unwrap());
        headers.insert(
            "x-forwarded-for",
            "192.0.2.1, 198.51.100.7".parse().unwrap(),
        );
        assert_eq!(source_ip(peer("127.0.0.1:4321"), &headers), "203.0.113.5");
        assert_eq!(
            source_ip(peer("[::ffff:127.0.0.1]:4321"), &headers),
            "203.0.113.5"
        );
        headers.remove("x-real-ip");
        assert_eq!(source_ip(peer("[::1]:4321"), &headers), "198.51.100.7");
        for invalid in ["malformed", "192.0.2.1, 198.51.100.7"] {
            headers.insert("x-real-ip", invalid.parse().unwrap());
            assert_eq!(source_ip(peer("127.0.0.1:4321"), &headers), "127.0.0.1");
        }
        assert_eq!(source_ip(peer("[::1]:4321"), &HeaderMap::new()), "::1");
    }

    async fn fixture() -> LoginThrottle {
        let url = std::env::var("TEST_REDIS_URL")
            .or_else(|_| std::env::var("REDIS_URL"))
            .unwrap_or_else(|_| "redis://127.0.0.1:6379/0".to_owned());
        let pool = crate::platform::connect_redis(&url).await.unwrap();
        LoginThrottle::new(pool, format!("test:{}:auth:login:", Uuid::new_v4()))
    }

    async fn cleanup(store: &LoginThrottle, keys: &[String]) {
        let mut conn = store.pool.get().await.unwrap();
        let _: u64 = conn.del(keys).await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_ip_attempts_are_atomic_and_do_not_extend_window() {
        let store = fixture().await;
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..150 {
            let store = store.clone();
            tasks.spawn(async move { store.check_ip("198.51.100.7").await });
        }
        let mut allowed = 0;
        let mut limited = 0;
        while let Some(result) = tasks.join_next().await {
            match result.unwrap() {
                Ok(()) => allowed += 1,
                Err(error) => {
                    assert_eq!(error.code(), ErrorCode::LoginRateLimited);
                    limited += 1;
                }
            }
        }
        assert_eq!((allowed, limited), (100, 50));
        let key = store.key("ip", "198.51.100.7");
        let mut conn = store.pool.get().await.unwrap();
        let ttl: i64 = conn.ttl(&key).await.unwrap();
        assert!((1..=900).contains(&ttl));
        let _: bool = conn.expire(&key, 20).await.unwrap();
        assert!(store.check_ip("198.51.100.7").await.is_err());
        let ttl: i64 = conn.ttl(&key).await.unwrap();
        assert!((1..=20).contains(&ttl), "拒绝不应续期");
        store.check_ip("198.51.100.8").await.unwrap();
        cleanup(&store, &[key, store.key("ip", "198.51.100.8")]).await;
    }

    #[tokio::test]
    async fn redis_failure_after_reservation_is_unavailable() {
        let store = fixture().await;
        let attempt = store.reserve("account").await.unwrap();
        // 关闭此 fixture 的池模拟凭据验证期间失联；不能把完成失败当成登录成功。
        let mut cleanup_conn = store.pool.get().await.unwrap();
        store.pool.close();
        let error = store.finish(attempt, Outcome::Success).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::LoginUnavailable);
        let error = store.reserve("account").await.err().unwrap();
        assert_eq!(error.code(), ErrorCode::LoginUnavailable);
        let _: u64 = cleanup_conn
            .del(store.key("account", "account"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn success_preserves_other_inflight_attempts_and_later_failures() {
        let store = fixture().await;
        let mut attempts = Vec::new();
        for _ in 0..10 {
            attempts.push(store.reserve("account").await.unwrap());
        }
        assert!(store.reserve("account").await.is_err());
        store
            .finish(attempts.pop().unwrap(), Outcome::Success)
            .await
            .unwrap();
        let replacement = store.reserve("account").await.unwrap();
        assert!(
            store.reserve("account").await.is_err(),
            "成功不能删除其他在途预约"
        );
        for attempt in attempts {
            store.finish(attempt, Outcome::Failure).await.unwrap();
        }
        store.finish(replacement, Outcome::Failure).await.unwrap();
        assert!(store.reserve("account").await.is_err());
        cleanup(&store, &[store.key("account", "account")]).await;
    }

    #[tokio::test]
    async fn expiry_releases_cancelled_attempts_and_old_completion_cannot_clear_new_window() {
        let store = fixture().await;
        let stale = store.reserve("account").await.unwrap();
        for _ in 1..10 {
            let _cancelled = store.reserve("account").await.unwrap();
        }
        store.check_ip("198.51.100.7").await.unwrap();
        let account_key = store.key("account", "account");
        let ip_key = store.key("ip", "198.51.100.7");
        let mut conn = store.pool.get().await.unwrap();
        let ttl: i64 = conn.ttl(&account_key).await.unwrap();
        assert!((1..=900).contains(&ttl));
        assert!(store.reserve("account").await.is_err());
        // 缩短本 fixture 的实际 Redis TTL，验证自然过期，不删除键模拟过期。
        for key in [&account_key, &ip_key] {
            let _: bool = cmd("PEXPIRE")
                .arg(key)
                .arg(30)
                .query_async(&mut conn)
                .await
                .unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let old_exists: bool = conn.exists(&account_key).await.unwrap();
        assert!(!old_exists);
        let ip_exists: bool = conn.exists(&ip_key).await.unwrap();
        assert!(!ip_exists);
        for _ in 0..10 {
            let attempt = store.reserve("account").await.unwrap();
            store.finish(attempt, Outcome::Failure).await.unwrap();
        }
        store.finish(stale, Outcome::Success).await.unwrap();
        assert!(store.reserve("account").await.is_err());
        for _ in 0..100 {
            store.check_ip("198.51.100.7").await.unwrap();
        }
        assert!(store.check_ip("198.51.100.7").await.is_err());
        cleanup(&store, &[account_key, ip_key]).await;
    }
}
