use std::time::Duration;

use deadpool_redis::Pool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const IMPACT_PREFIX: &str = "lexicon:forms-impact:";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactConfirmation {
    pub entry_id: Uuid,
    pub base_revision: i64,
    pub content_hash: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImpactStoreError {
    #[error(transparent)]
    Pool(#[from] deadpool_redis::PoolError),
    #[error(transparent)]
    Redis(#[from] deadpool_redis::redis::RedisError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct ImpactStore {
    redis: Pool,
}

impl ImpactStore {
    pub fn new(redis: Pool) -> Self {
        Self { redis }
    }

    pub async fn save(
        &self,
        actor_id: Uuid,
        token: Uuid,
        value: &ImpactConfirmation,
        ttl: Duration,
    ) -> Result<(), ImpactStoreError> {
        let mut connection = self.redis.get().await?;
        deadpool_redis::redis::cmd("SET")
            .arg(key(actor_id, token))
            .arg(serde_json::to_string(value)?)
            .arg("EX")
            .arg(ttl.as_secs() as i64)
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    pub async fn load(
        &self,
        actor_id: Uuid,
        token: Uuid,
    ) -> Result<Option<ImpactConfirmation>, ImpactStoreError> {
        let mut connection = self.redis.get().await?;
        let payload: Option<String> = deadpool_redis::redis::cmd("GET")
            .arg(key(actor_id, token))
            .query_async(&mut connection)
            .await?;
        payload
            .map(|payload| serde_json::from_str(&payload))
            .transpose()
            .map_err(Into::into)
    }
}

fn key(actor_id: Uuid, token: Uuid) -> String {
    format!("{IMPACT_PREFIX}{actor_id}:{token}")
}
