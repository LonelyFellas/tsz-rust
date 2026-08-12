use chrono::Duration;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use uuid::Uuid;

use crate::{
    auth::{Realm, TokenManager},
    otp::{sender::OtpSender, service::OtpService, store::OtpStore},
    platform::storage::StorageRegistry,
};
use axum::extract::FromRef;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub token_manager: Arc<TokenManager>,
    pub admin_token_manager: Arc<TokenManager>,
    pub refresh_ttl: Duration,
    pub admin_refresh_ttl: Duration,
    pub redis: deadpool_redis::Pool,
    pub otp_service: Arc<OtpService>,
    pub cookie_secure: bool,
    pub object_storage: StorageRegistry,
}

impl AppState {
    /// 集成测试用（**不关心 Redis** 的测试走这个）：塞 dummy TokenManager + 默认 TTL，
    /// 内部用默认串建一个**惰性** redis pool。deadpool 惰性建连——只要测试不真正触达
    /// Redis 就不会发起连接，故这类测试无需本地跑 Redis，也不必在调用处构造 pool。
    pub fn for_test(pool: PgPool) -> Self {
        let redis = deadpool_redis::Config::from_url("redis://127.0.0.1:6379/0")
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("测试 redis pool 应能创建（惰性，不立即连接）");
        Self::for_test_with_redis(pool, redis)
    }

    /// 需要**注入特定 redis pool** 的测试用——如 readyz 的「Redis 宕机」场景要塞一个
    /// 指向死地址的 pool 来验证探活会失败。
    pub fn for_test_with_redis(pool: PgPool, redis: deadpool_redis::Pool) -> Self {
        // 唯一前缀隔离——每个 for_test 的 OtpService 独享 keyspace，可并行、不串号。
        let otp_service = Arc::new(OtpService::new(
            OtpStore::with_prefix(redis.clone(), format!("test:{}:", Uuid::now_v7())),
            OtpSender::Mock,
            StdDuration::from_secs(60),  // cooldown（>0，好测 429）
            10,                          // daily_limit
            StdDuration::from_secs(300), // ttl
            5,                           // max_attempts
        ));
        Self {
            pool,
            token_manager: Arc::new(TokenManager::new(
                "test-secret",
                Realm::Web,
                Duration::minutes(15),
            )),
            admin_token_manager: Arc::new(TokenManager::new(
                "test-admin-secret",
                Realm::Admin,
                Duration::minutes(15),
            )),
            refresh_ttl: Duration::days(30),
            admin_refresh_ttl: Duration::days(7),
            redis,
            otp_service,
            cookie_secure: false,
            object_storage: StorageRegistry::empty(),
        }
    }

    /// 测试用：返回 `(AppState, OtpStore)`，二者共享**同一 OTP keyspace**（同 key 前缀）。
    /// 供 verify 编排的集成测试（如 login-otp）——先用返回的 `store` 注入一枚**已知** code，
    /// 再驱动 handler 走真实 `verify`。这里保留注入能力，以便测试任意验证码与异常分支，
    /// 不与 `OtpSender::Mock` 当前使用的固定码耦合。
    pub fn for_test_with_otp_store(pool: PgPool) -> (Self, OtpStore) {
        let redis = deadpool_redis::Config::from_url("redis://127.0.0.1:6379/0")
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("测试 redis pool 应能创建（惰性）");
        let prefix = format!("test:{}:", Uuid::now_v7());
        // 同一 prefix 造两个 handle：store 给测试注入码、service 给 handler verify——同 keyspace 才对得上。
        let store = OtpStore::with_prefix(redis.clone(), prefix.clone());
        let otp_service = Arc::new(OtpService::new(
            OtpStore::with_prefix(redis.clone(), prefix),
            OtpSender::Mock,
            StdDuration::from_secs(60),
            10,
            StdDuration::from_secs(300),
            5,
        ));
        let state = Self {
            pool,
            cookie_secure: false,
            token_manager: Arc::new(TokenManager::new(
                "test-secret",
                Realm::Web,
                Duration::minutes(15),
            )),
            admin_token_manager: Arc::new(TokenManager::new(
                "test-admin-secret",
                Realm::Admin,
                Duration::minutes(15),
            )),
            refresh_ttl: Duration::days(30),
            admin_refresh_ttl: Duration::days(7),
            redis,
            otp_service,
            object_storage: StorageRegistry::empty(),
        };
        (state, store)
    }
}

// 让只要 pool 的老 handler（healthz/readyz/register）继续用 State<PgPool>，不用改
impl FromRef<AppState> for PgPool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}

impl FromRef<AppState> for deadpool_redis::Pool {
    fn from_ref(state: &AppState) -> Self {
        state.redis.clone()
    }
}

impl FromRef<AppState> for StorageRegistry {
    fn from_ref(state: &AppState) -> Self {
        state.object_storage.clone()
    }
}
