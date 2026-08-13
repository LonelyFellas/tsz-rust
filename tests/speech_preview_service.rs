use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use sqlx::PgPool;
use tsz_rust::{
    lexicon::dto::RichTextV2,
    platform::storage::{MemoryAdapter, ObjectStore, StoragePolicy, StoragePrivacy, StorageSpace},
    speech::{
        SpeechError, SpeechProvider, SynthesisRequest, SynthesizedAudio,
        preview::{
            PreviewRepository, PreviewService,
            dto::{CreatePreviewRequest, PreviewCacheStatus},
        },
    },
};
use uuid::Uuid;

struct FakeProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl SpeechProvider for FakeProvider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }

    async fn synthesize(
        &self,
        _request: &SynthesisRequest,
    ) -> Result<SynthesizedAudio, SpeechError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(SynthesizedAudio {
            bytes: b"mp3".to_vec(),
            content_type: "audio/mpeg",
            provider_request_id: None,
        })
    }
}

fn redis_pool() -> deadpool_redis::Pool {
    deadpool_redis::Config::from_url("redis://127.0.0.1:6379/0")
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap()
}

fn request() -> CreatePreviewRequest {
    CreatePreviewRequest {
        content: RichTextV2 {
            version: 2,
            text: "hello".to_owned(),
            annotations: vec![],
        },
        voice_alias: "en-us-jenny".to_owned(),
        style: Some("chat".to_owned()),
        rate_percent: 0,
        pitch_semitones: 0,
    }
}

async fn insert_voice(pool: &PgPool) {
    sqlx::query(
        r#"INSERT INTO speech.voices
           (id, alias, provider, provider_voice_id, locale, gender, styles, provider_version)
           VALUES ($1, 'en-us-jenny', 'azure', 'en-US-JennyNeural', 'en-US', 'female', '["chat"]', 'v1')"#,
    ).bind(Uuid::now_v7()).execute(pool).await.unwrap();
}

#[sqlx::test]
async fn preview_generation_then_hash_hit_calls_provider_once(pool: PgPool) {
    insert_voice(&pool).await;
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    let store: Arc<dyn ObjectStore> = MemoryAdapter::object_store(
        StorageSpace::parse("speech").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    );
    let service = PreviewService::new(
        PreviewRepository::new(pool),
        redis_pool(),
        Some(provider.clone()),
        Some(store),
    );

    let generated = service.create_preview(request()).await.unwrap();
    assert!(matches!(
        generated.cache_status,
        PreviewCacheStatus::Generated
    ));
    assert_eq!(generated.url_expires_in_seconds, 60);
    let hit = service.create_preview(request()).await.unwrap();
    assert!(matches!(hit.cache_status, PreviewCacheStatus::Hit));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[sqlx::test]
async fn voice_listing_hides_provider_identity_and_disabled_rows(pool: PgPool) {
    insert_voice(&pool).await;
    sqlx::query(
        r#"INSERT INTO speech.voices
           (id, alias, provider, provider_voice_id, locale, gender, provider_version, enabled)
           VALUES ($1, 'disabled', 'azure', 'secret-provider-id', 'en-US', 'male', 'v1', false)"#,
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
    let service = PreviewService::new(PreviewRepository::new(pool), redis_pool(), None, None);
    let response = service.list_voices().await.unwrap();
    assert_eq!(response.items.len(), 1);
    assert_eq!(response.items[0].alias, "en-us-jenny");
    assert_eq!(response.items[0].capabilities.styles, vec!["chat"]);
}

#[sqlx::test]
async fn concurrent_same_fingerprint_has_one_provider_owner(pool: PgPool) {
    insert_voice(&pool).await;
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    let store: Arc<dyn ObjectStore> = MemoryAdapter::object_store(
        StorageSpace::parse("speech").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    );
    let service = PreviewService::new(
        PreviewRepository::new(pool),
        redis_pool(),
        Some(provider.clone()),
        Some(store),
    );
    let first = service.clone();
    let second = service;
    let (left, right) = tokio::join!(
        async move { first.create_preview(request()).await },
        async move { second.create_preview(request()).await },
    );
    assert!(left.is_ok(), "first request should finish");
    assert!(right.is_ok(), "second request should hit winner");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}
