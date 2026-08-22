use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use deadpool_redis::Pool as RedisPool;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::Instrument;

use crate::{
    platform::storage::{ObjectContentType, ObjectKey, ObjectStore, PutOptions, StorageError},
    speech::{SpeechError, SpeechModelError, SpeechOptions, SpeechProvider, SynthesisRequest},
};

use super::{
    CACHE_TTL_HOURS,
    dto::{CreatePreviewRequest, PreviewCacheStatus, PreviewResponse, VoiceListResponse},
    lock::{PreviewLock, wait_interval},
    repository::{CacheRecord, PreviewRepositoryPort},
};

#[derive(Debug, Error)]
pub enum PreviewServiceError {
    #[error("voice not found")]
    VoiceNotFound,
    #[error("invalid speech preview request")]
    InvalidRequest(#[from] SpeechModelError),
    #[error("speech preview is already being generated")]
    InProgress,
    #[error("speech provider is not configured")]
    ProviderNotConfigured,
    #[error("speech provider does not match voice catalog")]
    ProviderMismatch,
    #[error("speech provider failed")]
    Provider(#[from] SpeechError),
    #[error("speech storage failed")]
    Storage(#[from] StorageError),
    #[error("speech database failed")]
    Database(#[from] sqlx::Error),
    #[error("speech lock failed")]
    Lock,
    #[error("speech preview task failed")]
    Task(#[from] tokio::task::JoinError),
}

#[derive(Clone)]
pub struct PreviewService {
    repository: Arc<dyn PreviewRepositoryPort>,
    redis: RedisPool,
    provider: Option<Arc<dyn SpeechProvider>>,
    storage: Option<Arc<dyn ObjectStore>>,
}

impl PreviewService {
    pub fn new<R>(
        repository: R,
        redis: RedisPool,
        provider: Option<Arc<dyn SpeechProvider>>,
        storage: Option<Arc<dyn ObjectStore>>,
    ) -> Self
    where
        R: PreviewRepositoryPort + 'static,
    {
        Self {
            repository: Arc::new(repository),
            redis,
            provider,
            storage,
        }
    }

    pub async fn list_voices(&self) -> Result<VoiceListResponse, PreviewServiceError> {
        Ok(self.repository.list_voices().await?)
    }

    pub async fn create_preview(
        &self,
        input: CreatePreviewRequest,
    ) -> Result<PreviewResponse, PreviewServiceError> {
        let voice = self
            .repository
            .voice_by_alias(&input.voice_alias)
            .await?
            .ok_or(PreviewServiceError::VoiceNotFound)?;
        if input.rate_percent < voice.min_rate_percent
            || input.rate_percent > voice.max_rate_percent
            || i16::from(input.pitch_semitones) < voice.min_pitch_semitones
            || i16::from(input.pitch_semitones) > voice.max_pitch_semitones
        {
            return Err(PreviewServiceError::InvalidRequest(
                SpeechModelError::InvalidRichText,
            ));
        }
        let options = SpeechOptions::new(
            &voice.voice,
            input.style,
            input.rate_percent,
            input.pitch_semitones,
        )?;
        let request = SynthesisRequest::new(voice.voice, options, input.content)?;
        let request_hash =
            versioned_hash(request.fingerprint().as_bytes(), &voice.provider_version);
        let content_hash: [u8; 32] = Sha256::digest(request.normalized_content()).into();
        let storage = self.storage.clone().ok_or(PreviewServiceError::Storage(
            StorageError::SpaceNotConfigured(
                "speech".parse().expect("constant storage space is valid"),
            ),
        ))?;

        if let Some(cache) = self.repository.active_cache(&request_hash).await? {
            return signed_response(&storage, cache, PreviewCacheStatus::Hit).await;
        }

        let lease = self
            .provider
            .as_ref()
            .map_or(Duration::from_secs(30), |provider| {
                provider
                    .synthesis_timeout()
                    .saturating_add(Duration::from_secs(30))
            });
        let Some(lock) = PreviewLock::acquire(self.redis.clone(), &request_hash, lease)
            .await
            .map_err(|_| PreviewServiceError::Lock)?
        else {
            for _ in 0..10 {
                wait_interval().await;
                if let Some(cache) = self.repository.active_cache(&request_hash).await? {
                    return signed_response(&storage, cache, PreviewCacheStatus::Hit).await;
                }
            }
            return Err(PreviewServiceError::InProgress);
        };

        // 生成过程 detach 到独立任务：客户端中途 abort（改文本、换发音人、关页面）时
        // axum 会丢弃 handler future，若生成就地跑就会在 put 与 save_cache 之间留下无 DB 行的
        // 孤儿对象，且 lock.release() 永远不会执行，同一 fingerprint 要等租约到期才能再试。
        // detach 后任务照常跑完：已经付过费的合成结果被写进缓存，锁也一定释放。
        let voice_id = voice.id;
        let service = self.clone();
        // 带上当前 span：任务跑在 handler 之外，不继承的话生成路径上的告警会丢掉 request_id。
        let generation = tokio::spawn(
            async move {
                let result = service
                    .generate_locked(&request_hash, &content_hash, voice_id, &request, storage)
                    .await;
                if let Err(error) = lock.release().await {
                    tracing::warn!(
                        error_kind = "redis_unlock",
                        "speech preview lock release failed"
                    );
                    let _ = error;
                }
                result
            }
            .instrument(tracing::Span::current()),
        );
        generation.await?
    }

    async fn generate_locked(
        &self,
        request_hash: &[u8; 32],
        content_hash: &[u8; 32],
        voice_id: uuid::Uuid,
        request: &SynthesisRequest,
        storage: Arc<dyn ObjectStore>,
    ) -> Result<PreviewResponse, PreviewServiceError> {
        if let Some(cache) = self.repository.active_cache(request_hash).await? {
            return signed_response(&storage, cache, PreviewCacheStatus::Hit).await;
        }
        let provider = self
            .provider
            .as_ref()
            .ok_or(PreviewServiceError::ProviderNotConfigured)?;
        if provider.provider_name() != request.voice().provider() {
            return Err(PreviewServiceError::ProviderMismatch);
        }
        let audio = provider.synthesize(request).await?;
        if audio.bytes.is_empty() || audio.content_type != "audio/mpeg" {
            return Err(PreviewServiceError::Provider(SpeechError::new(
                crate::speech::SpeechErrorKind::InvalidResponse,
                None,
            )));
        }
        let stale = self.repository.cache_by_hash(request_hash).await?;
        let key = ObjectKey::generate("previews", Some("mp3"))
            .expect("constant speech preview namespace and extension are valid");
        let size_bytes = i64::try_from(audio.bytes.len())
            .map_err(|_| PreviewServiceError::InvalidRequest(SpeechModelError::InvalidRichText))?;
        storage
            .put(
                &key,
                audio.bytes,
                PutOptions::new(Some(
                    ObjectContentType::parse("audio/mpeg").expect("constant mime is valid"),
                )),
            )
            .await?;

        let stored = self
            .repository
            .save_cache(
                request_hash,
                content_hash,
                voice_id,
                key.as_str(),
                size_bytes,
            )
            .await;
        match stored {
            Ok(Some(_)) => {
                if let Some(stale) = stale
                    && stale.object_key != key.as_str()
                    && let Ok(stale_key) = ObjectKey::parse(stale.object_key)
                {
                    compensate_delete(&storage, &stale_key).await;
                }
                signed_response(
                    &storage,
                    CacheRecord {
                        object_key: key.to_string(),
                        expires_at: Utc::now() + chrono::Duration::hours(CACHE_TTL_HOURS),
                    },
                    PreviewCacheStatus::Generated,
                )
                .await
            }
            Ok(None) => {
                compensate_delete(&storage, &key).await;
                let cache = self
                    .repository
                    .active_cache(request_hash)
                    .await?
                    .ok_or(PreviewServiceError::InProgress)?;
                signed_response(&storage, cache, PreviewCacheStatus::Hit).await
            }
            Err(error) => {
                compensate_delete(&storage, &key).await;
                Err(error.into())
            }
        }
    }
}

async fn signed_response(
    storage: &Arc<dyn ObjectStore>,
    cache: CacheRecord,
    cache_status: PreviewCacheStatus,
) -> Result<PreviewResponse, PreviewServiceError> {
    let key = ObjectKey::parse(cache.object_key)
        .map_err(|error| PreviewServiceError::Database(sqlx::Error::Decode(Box::new(error))))?;
    let signed = storage.presign_read(&key).await?;
    let ttl = signed.expires_in();
    let expires_at = DateTime::<Utc>::from(std::time::SystemTime::now() + ttl);
    Ok(PreviewResponse {
        cache_status,
        audio_url: signed.url().to_owned(),
        expires_at,
        url_expires_in_seconds: ttl.as_secs(),
    })
}

async fn compensate_delete(storage: &Arc<dyn ObjectStore>, key: &ObjectKey) {
    if storage.delete(key).await.is_err() {
        tracing::warn!(object_key = %key, error_kind = "storage_delete", "speech preview compensation failed");
    }
}

fn versioned_hash(fingerprint: &[u8; 32], provider_version: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update((fingerprint.len() as u64).to_be_bytes());
    hasher.update(fingerprint);
    hasher.update((provider_version.len() as u64).to_be_bytes());
    hasher.update(provider_version.as_bytes());
    hasher.finalize().into()
}
