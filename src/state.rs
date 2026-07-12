use chrono::Duration;
use sqlx::PgPool;
use std::sync::Arc;

use crate::auth::{Realm, TokenManager};
use axum::extract::FromRef;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub token_manager: Arc<TokenManager>,
    pub refresh_ttl: Duration,
    pub redis: deadpool_redis::Pool,
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
        Self {
            pool,
            token_manager: Arc::new(TokenManager::new(
                "test-secret",
                Realm::Web,
                Duration::minutes(15),
            )),
            refresh_ttl: Duration::days(30),
            redis,
        }
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
