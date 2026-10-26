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
    #[error(transparent)]
    Authorization(#[from] crate::error::AppError),
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
    authorization_pool: sqlx::PgPool,
    repository: Arc<dyn PreviewRepositoryPort>,
    redis: RedisPool,
    provider: Option<Arc<dyn SpeechProvider>>,
    storage: Option<Arc<dyn ObjectStore>>,
    test_run: Option<String>,
}

impl PreviewService {
    pub fn new<R>(
        authorization_pool: sqlx::PgPool,
        repository: R,
        redis: RedisPool,
        provider: Option<Arc<dyn SpeechProvider>>,
        storage: Option<Arc<dyn ObjectStore>>,
    ) -> Self
    where
        R: PreviewRepositoryPort + 'static,
    {
        Self {
            authorization_pool,
            repository: Arc::new(repository),
            redis,
            provider,
            storage,
            test_run: None,
        }
    }

    pub fn with_test_run(mut self, run: Option<String>) -> Self {
        self.test_run = run;
        self
    }

    async fn authorize(
        &self,
        actor_id: uuid::Uuid,
        generate: bool,
    ) -> Result<(), PreviewServiceError> {
        let mut tx = self.authorization_pool.begin().await?;
        let authorization = crate::admin::permissions::lock(&mut tx, actor_id).await?;
        if generate {
            authorization.require("words.access")?;
            authorization.require("speech.generate")?;
        } else if !authorization.has("words.access") && !authorization.has("speech.global_dubbing")
        {
            authorization.require("sentences.access")?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_voices(
        &self,
        actor_id: uuid::Uuid,
    ) -> Result<VoiceListResponse, PreviewServiceError> {
        self.authorize(actor_id, false).await?;
        Ok(self.repository.list_voices().await?)
    }

    pub async fn create_preview(
        &self,
        actor_id: uuid::Uuid,
        input: CreatePreviewRequest,
    ) -> Result<PreviewResponse, PreviewServiceError> {
        self.authorize(actor_id, true).await?;
        let voice = self
            .repository
            .voice_by_alias(&input.voice_alias)
            .await?
            .ok_or(PreviewServiceError::VoiceNotFound)?;
        if !crate::speech::capabilities::synthesis_available(
            voice.voice.provider_voice_id(),
            voice.voice.locale(),
        ) {
            return Err(PreviewServiceError::InvalidRequest(
                SpeechModelError::UnsupportedModelCapability,
            ));
        }
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
            input.volume_percent.unwrap_or(100),
        )?
        .with_expression(&voice.voice, input.style_degree, input.role)?
        .with_output_format(input.output_format.unwrap_or_default());
        let request = SynthesisRequest::new(voice.voice, options, input.content)?;
        self.create_prepared(actor_id, voice.id, &voice.provider_version, request)
            .await
    }

    pub async fn create_manual_preview(
        &self,
        actor_id: uuid::Uuid,
        input: super::dto::ManualPreviewRequest,
    ) -> Result<PreviewResponse, PreviewServiceError> {
        self.authorize(actor_id, true).await?;
        let voice = self
            .repository
            .voice_by_alias(&input.voice_alias)
            .await?
            .ok_or(PreviewServiceError::VoiceNotFound)?;
        if !crate::speech::capabilities::synthesis_available(
            voice.voice.provider_voice_id(),
            voice.voice.locale(),
        ) {
            return Err(SpeechModelError::UnsupportedModelCapability.into());
        }
        let request = crate::speech::manual_ssml::request(
            voice.voice,
            input.ssml,
            input.output_format,
            (voice.min_rate_percent, voice.max_rate_percent),
            (voice.min_pitch_semitones, voice.max_pitch_semitones),
        )?;
        self.create_prepared(actor_id, voice.id, &voice.provider_version, request)
            .await
    }

    async fn create_prepared(
        &self,
        actor_id: uuid::Uuid,
        voice_id: uuid::Uuid,
        provider_version: &str,
        request: SynthesisRequest,
    ) -> Result<PreviewResponse, PreviewServiceError> {
        let request_hash = scoped_hash(
            request.fingerprint().as_bytes(),
            provider_version,
            self.test_run.as_deref(),
        );
        let content_hash: [u8; 32] = Sha256::digest(request.normalized_content()).into();
        let storage = self.storage.clone().ok_or(PreviewServiceError::Storage(
            StorageError::SpaceNotConfigured(
                "speech".parse().expect("constant storage space is valid"),
            ),
        ))?;

        if let Some(cache) = self.repository.active_cache(&request_hash).await? {
            return self
                .signed_response(
                    actor_id,
                    &storage,
                    cache,
                    PreviewCacheStatus::Hit,
                    &request_hash,
                )
                .await;
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
                    return self
                        .signed_response(
                            actor_id,
                            &storage,
                            cache,
                            PreviewCacheStatus::Hit,
                            &request_hash,
                        )
                        .await;
                }
            }
            return Err(PreviewServiceError::InProgress);
        };

        // 生成过程 detach 到独立任务：客户端中途 abort（改文本、换发音人、关页面）时
        // axum 会丢弃 handler future，若生成就地跑就会在 put 与 save_cache 之间留下无 DB 行的
        // 孤儿对象，且 lock.release() 永远不会执行，同一 fingerprint 要等租约到期才能再试。
        // detach 后任务照常跑完：已经付过费的合成结果被写进缓存，锁也一定释放。
        let service = self.clone();
        // 带上当前 span：任务跑在 handler 之外，不继承的话生成路径上的告警会丢掉 request_id。
        let generation = tokio::spawn(
            async move {
                let result = service
                    .generate_locked(
                        actor_id,
                        &request_hash,
                        &content_hash,
                        voice_id,
                        &request,
                        storage,
                    )
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
        actor_id: uuid::Uuid,
        request_hash: &[u8; 32],
        content_hash: &[u8; 32],
        voice_id: uuid::Uuid,
        request: &SynthesisRequest,
        storage: Arc<dyn ObjectStore>,
    ) -> Result<PreviewResponse, PreviewServiceError> {
        if let Some(cache) = self.repository.active_cache(request_hash).await? {
            return self
                .signed_response(
                    actor_id,
                    &storage,
                    cache,
                    PreviewCacheStatus::Hit,
                    request_hash,
                )
                .await;
        }
        let provider = self
            .provider
            .as_ref()
            .ok_or(PreviewServiceError::ProviderNotConfigured)?;
        if provider.provider_name() != request.voice().provider() {
            return Err(PreviewServiceError::ProviderMismatch);
        }
        self.authorize(actor_id, true).await?;
        let audio = provider.synthesize(request).await?;
        if audio.bytes.is_empty()
            || audio.content_type != request.options().output_format().content_type()
            || (request.options().output_format()
                == crate::speech::AudioOutputFormat::Wav24Khz16BitMono
                && !request.options().output_format().accepts(&audio.bytes))
        {
            return Err(PreviewServiceError::Provider(SpeechError::new(
                crate::speech::SpeechErrorKind::InvalidResponse,
                None,
            )));
        }
        let stale = self.repository.cache_by_hash(request_hash).await?;
        let key = storage
            .qualify_key(
                ObjectKey::generate(
                    "previews",
                    Some(request.options().output_format().extension()),
                )
                .expect("constant speech preview namespace and extension are valid"),
            )
            .expect("validated test prefix fits generated key");
        let size_bytes = i64::try_from(audio.bytes.len())
            .map_err(|_| PreviewServiceError::InvalidRequest(SpeechModelError::InvalidRichText))?;
        self.authorize(actor_id, true).await?;
        storage
            .put(
                &key,
                audio.bytes,
                PutOptions::new(Some(
                    ObjectContentType::parse(request.options().output_format().content_type())
                        .expect("constant mime is valid"),
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
                self.signed_response(
                    actor_id,
                    &storage,
                    CacheRecord {
                        object_key: key.to_string(),
                        expires_at: Utc::now() + chrono::Duration::hours(CACHE_TTL_HOURS),
                    },
                    PreviewCacheStatus::Generated,
                    request_hash,
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
                self.signed_response(
                    actor_id,
                    &storage,
                    cache,
                    PreviewCacheStatus::Hit,
                    request_hash,
                )
                .await
            }
            Err(error) => {
                compensate_delete(&storage, &key).await;
                Err(error.into())
            }
        }
    }

    pub async fn read_content(
        &self,
        actor: uuid::Uuid,
        hash: &[u8; 32],
    ) -> Result<(String, Vec<u8>), PreviewServiceError> {
        self.authorize(actor, true).await?;
        let cache = self
            .repository
            .active_cache(hash)
            .await?
            .ok_or(PreviewServiceError::VoiceNotFound)?;
        let key = ObjectKey::parse(cache.object_key)
            .map_err(|_| PreviewServiceError::InvalidRequest(SpeechModelError::InvalidRichText))?;
        let bytes = self
            .storage
            .as_ref()
            .ok_or(PreviewServiceError::ProviderNotConfigured)?
            .read(&key)
            .await?;
        self.authorize(actor, true).await?;
        if self.repository.active_cache(hash).await?.is_none() {
            return Err(PreviewServiceError::VoiceNotFound);
        }
        Ok((
            if key.as_str().ends_with(".wav") {
                "audio/wav"
            } else {
                "audio/mpeg"
            }
            .into(),
            bytes,
        ))
    }

    async fn signed_response(
        &self,
        actor_id: uuid::Uuid,
        storage: &Arc<dyn ObjectStore>,
        cache: CacheRecord,
        cache_status: PreviewCacheStatus,
        request_hash: &[u8; 32],
    ) -> Result<PreviewResponse, PreviewServiceError> {
        self.authorize(actor_id, true).await?;
        let key = ObjectKey::parse(cache.object_key)
            .map_err(|error| PreviewServiceError::Database(sqlx::Error::Decode(Box::new(error))))?;
        let signed = storage.presign_read(&key).await?;
        self.authorize(actor_id, true).await?;
        let ttl = signed.expires_in();
        let expires_at = DateTime::<Utc>::from(std::time::SystemTime::now() + ttl);
        Ok(PreviewResponse {
            content_path: format!(
                "/speech/previews/{}/content",
                request_hash
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ),
            cache_status,
            audio_url: signed.url().to_owned(),
            expires_at,
            url_expires_in_seconds: ttl.as_secs(),
        })
    }
}

async fn compensate_delete(storage: &Arc<dyn ObjectStore>, key: &ObjectKey) {
    if storage.delete(key).await.is_err() {
        tracing::warn!(
            error_kind = "storage_delete",
            "speech preview compensation failed"
        );
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

fn scoped_hash(fingerprint: &[u8; 32], version: &str, run: Option<&str>) -> [u8; 32] {
    let normal = versioned_hash(fingerprint, version);
    match run {
        None => normal,
        Some(run) => versioned_hash(&normal, &format!("temporary-tests/{run}")),
    }
}

#[cfg(test)]
mod test_scope_hash_tests {
    use super::*;
    #[test]
    fn preview_cache_preserves_normal_fingerprints_and_isolates_each_test_batch() {
        let fingerprint = [42; 32];
        let normal = versioned_hash(&fingerprint, "azure-v1");
        assert_eq!(scoped_hash(&fingerprint, "azure-v1", None), normal);
        let first = scoped_hash(&fingerprint, "azure-v1", Some("fixture-20261017"));
        assert_ne!(first, normal);
        assert_ne!(
            first,
            scoped_hash(&fingerprint, "azure-v1", Some("another-20261017"))
        );
        assert_eq!(
            first,
            scoped_hash(&fingerprint, "azure-v1", Some("fixture-20261017"))
        );
    }
}
