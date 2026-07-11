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
}

impl AppState {
    /// 集成测试用：塞一个 dummy secret 的 web TokenManager + 默认 TTL。
    /// 让测试只需给 pool 就能拿到能跑 `router()` 的 state。
    pub fn for_test(pool: PgPool) -> Self {
        Self {
            pool,
            token_manager: Arc::new(TokenManager::new(
                "test-secret",
                Realm::Web,
                Duration::minutes(15),
            )),
            refresh_ttl: Duration::days(30),
        }
    }
}

// 让只要 pool 的老 handler（healthz/readyz/register）继续用 State<PgPool>，不用改
impl FromRef<AppState> for PgPool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}
