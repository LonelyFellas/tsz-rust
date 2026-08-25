use std::time::Duration;

use deadpool_redis::Pool;
use uuid::Uuid;

use crate::lexicon::dto::{DetectLexiconSurfaceResponseV3, DetectWordResponseV2};

const DETECTION_PREFIX: &str = "lexicon:detection:";

#[derive(Debug, thiserror::Error)]
pub enum DetectionStoreError {
    #[error(transparent)]
    Pool(#[from] deadpool_redis::PoolError),
    #[error(transparent)]
    Redis(#[from] deadpool_redis::redis::RedisError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct DetectionStore {
    redis: Pool,
}

impl DetectionStore {
    pub fn new(redis: Pool) -> Self {
        Self { redis }
    }

    pub async fn save(
        &self,
        actor_id: Uuid,
        detection: &DetectWordResponseV2,
        ttl: Duration,
    ) -> Result<(), DetectionStoreError> {
        self.save_payload(actor_id, detection.detection_id, detection, ttl)
            .await
    }

    pub async fn save_v3(
        &self,
        actor_id: Uuid,
        detection: &DetectLexiconSurfaceResponseV3,
        ttl: Duration,
    ) -> Result<(), DetectionStoreError> {
        self.save_payload(actor_id, detection.detection_id, detection, ttl)
            .await
    }

    async fn save_payload<T: serde::Serialize>(
        &self,
        actor_id: Uuid,
        detection_id: Uuid,
        detection: &T,
        ttl: Duration,
    ) -> Result<(), DetectionStoreError> {
        let mut connection = self.redis.get().await?;
        let payload = serde_json::to_string(detection)?;
        deadpool_redis::redis::cmd("SET")
            .arg(key(actor_id, detection_id))
            .arg(payload)
            .arg("EX")
            .arg(ttl.as_secs() as i64)
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    pub async fn load(
        &self,
        actor_id: Uuid,
        detection_id: Uuid,
    ) -> Result<Option<DetectWordResponseV2>, DetectionStoreError> {
        let mut connection = self.redis.get().await?;
        let payload: Option<String> = deadpool_redis::redis::cmd("GET")
            .arg(key(actor_id, detection_id))
            .query_async(&mut connection)
            .await?;
        payload
            .map(|payload| {
                let mut value: serde_json::Value = serde_json::from_str(&payload)?;
                // Pre-versioned V2 detections may still be alive in Redis during a rolling
                // upgrade. Keep that compatibility inside the cache reader; the public V2
                // response contract itself always requires literal schema_version=2.
                if let Some(object) = value.as_object_mut() {
                    object
                        .entry("schema_version")
                        .or_insert(serde_json::Value::from(2));
                }
                serde_json::from_value(value)
            })
            .transpose()
            .map_err(Into::into)
    }

    pub async fn load_v3(
        &self,
        actor_id: Uuid,
        detection_id: Uuid,
    ) -> Result<Option<DetectLexiconSurfaceResponseV3>, DetectionStoreError> {
        let mut connection = self.redis.get().await?;
        let payload: Option<String> = deadpool_redis::redis::cmd("GET")
            .arg(key(actor_id, detection_id))
            .query_async(&mut connection)
            .await?;
        payload
            .map(|payload| serde_json::from_str(&payload))
            .transpose()
            .map_err(Into::into)
    }

    pub async fn remove(
        &self,
        actor_id: Uuid,
        detection_id: Uuid,
    ) -> Result<(), DetectionStoreError> {
        let mut connection = self.redis.get().await?;
        deadpool_redis::redis::cmd("DEL")
            .arg(key(actor_id, detection_id))
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }
}

fn key(actor_id: Uuid, detection_id: Uuid) -> String {
    format!("{DETECTION_PREFIX}{actor_id}:{detection_id}")
}
