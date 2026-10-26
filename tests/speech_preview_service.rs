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
        request: &SynthesisRequest,
    ) -> Result<SynthesizedAudio, SpeechError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let format = request.options().output_format();
        let bytes = if format == tsz_rust::speech::AudioOutputFormat::Wav24Khz16BitMono {
            let mut bytes = vec![0; 48];
            bytes[..4].copy_from_slice(b"RIFF");
            bytes[4..8].copy_from_slice(&40u32.to_le_bytes());
            bytes[8..16].copy_from_slice(b"WAVEfmt ");
            bytes[16..20].copy_from_slice(&16u32.to_le_bytes());
            bytes[20..22].copy_from_slice(&1u16.to_le_bytes());
            bytes[22..24].copy_from_slice(&1u16.to_le_bytes());
            bytes[24..28].copy_from_slice(&24000u32.to_le_bytes());
            bytes[28..32].copy_from_slice(&48000u32.to_le_bytes());
            bytes[32..34].copy_from_slice(&2u16.to_le_bytes());
            bytes[34..36].copy_from_slice(&16u16.to_le_bytes());
            bytes[36..40].copy_from_slice(b"data");
            bytes[40..44].copy_from_slice(&4u32.to_le_bytes());
            bytes
        } else {
            b"ID3-fixture".to_vec()
        };
        Ok(SynthesizedAudio {
            bytes,
            content_type: format.content_type(),
            provider_request_id: None,
        })
    }
}

async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    tsz_rust::admin::AdminRepository::new(pool.clone())
        .create(tsz_rust::admin::NewAdmin {
            id,
            phone: format!("speech-{}", id.simple()),
            display_name: "Speech Admin".to_owned(),
            password_hash: "hash".to_owned(),
            role: tsz_rust::admin::AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_permission_grants (admin_id, permission_key, granted_by) SELECT $1, unnest(ARRAY['words.access', 'speech.generate']), $1")
        .bind(id).execute(pool).await.unwrap();
    id
}

fn redis_pool() -> deadpool_redis::Pool {
    deadpool_redis::Config::from_url(
        std::env::var("TEST_REDIS_URL").expect("isolated test Redis required"),
    )
    .create_pool(Some(deadpool_redis::Runtime::Tokio1))
    .unwrap()
}

fn request(text: &str) -> CreatePreviewRequest {
    CreatePreviewRequest {
        output_format: None,
        content: RichTextV2 {
            version: 2,
            text: text.to_owned(),
            annotations: vec![],
        },
        voice_alias: "en-us-jenny".to_owned(),
        style: Some("chat".to_owned()),
        style_degree: None,
        role: None,
        rate_percent: 0,
        pitch_semitones: 0,
        volume_percent: None,
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
async fn unknown_capabilities_and_unconfirmed_expression_never_call_provider(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    insert_voice(&pool).await;
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    let service = PreviewService::new(
        pool.clone(),
        PreviewRepository::new(pool.clone()),
        redis_pool(),
        Some(provider.clone()),
        None,
    );
    for degree in [0.0, 0.73, 2.01] {
        let mut input = request("degree");
        input.style_degree = Some(degree);
        assert!(service.create_preview(admin_id, input).await.is_err());
    }
    let mut input = request("role");
    input.role = Some("YoungAdultFemale".into());
    assert!(service.create_preview(admin_id, input).await.is_err());
    sqlx::query("UPDATE speech.voices SET provider_voice_id='en-US-UnknownNeural' WHERE alias='en-us-jenny'").execute(&pool).await.unwrap();
    assert!(
        service
            .create_preview(admin_id, request("unknown"))
            .await
            .is_err()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[sqlx::test]
async fn preview_generation_then_hash_hit_calls_provider_once(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    insert_voice(&pool).await;
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    let store: Arc<dyn ObjectStore> = MemoryAdapter::object_store(
        StorageSpace::parse("speech").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    );
    let service = PreviewService::new(
        pool.clone(),
        PreviewRepository::new(pool),
        redis_pool(),
        Some(provider.clone()),
        Some(store),
    );

    let generated = service
        .create_preview(admin_id, request("cache-hit-test"))
        .await
        .unwrap();
    assert!(matches!(
        generated.cache_status,
        PreviewCacheStatus::Generated
    ));
    assert_eq!(generated.url_expires_in_seconds, 60);
    let hit = service
        .create_preview(admin_id, request("cache-hit-test"))
        .await
        .unwrap();
    assert!(matches!(hit.cache_status, PreviewCacheStatus::Hit));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let mut quieter = request("cache-hit-test");
    quieter.volume_percent = Some(65);
    let adjusted = service.create_preview(admin_id, quieter).await.unwrap();
    assert!(matches!(
        adjusted.cache_status,
        PreviewCacheStatus::Generated
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let mut quieter = request("cache-hit-test");
    quieter.volume_percent = Some(65);
    let hit = service.create_preview(admin_id, quieter).await.unwrap();
    assert!(matches!(hit.cache_status, PreviewCacheStatus::Hit));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}

#[sqlx::test]
async fn preview_formats_and_manual_ssml_have_distinct_cache_and_read_actual_bytes(pool: PgPool) {
    use tsz_rust::speech::{AudioOutputFormat, preview::dto::ManualPreviewRequest};
    let actor = seed_admin(&pool).await;
    insert_voice(&pool).await;
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    let storage = MemoryAdapter::object_store(
        StorageSpace::parse("speech").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    );
    let service = PreviewService::new(
        pool.clone(),
        PreviewRepository::new(pool.clone()),
        redis_pool(),
        Some(provider.clone()),
        Some(storage),
    );
    let mp3 = service
        .create_preview(actor, request("formats"))
        .await
        .unwrap();
    let mut input = request("formats");
    input.output_format = Some(AudioOutputFormat::Wav24Khz16BitMono);
    let wav = service.create_preview(actor, input).await.unwrap();
    assert_ne!(mp3.content_path, wav.content_path);
    let hash_string = wav.content_path.split('/').nth(3).unwrap();
    let mut hash = [0; 32];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hash_string[index * 2..index * 2 + 2], 16).unwrap();
    }
    let (mime, bytes) = service.read_content(actor, &hash).await.unwrap();
    assert_eq!(mime, "audio/wav");
    assert_eq!(&bytes[..4], b"RIFF");
    let mime: String =
        sqlx::query_scalar("SELECT mime_type FROM speech.preview_cache WHERE request_hash=$1")
            .bind(hash.as_slice())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(mime, "audio/wav");
    let raw = r#" <speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" xml:lang="en-US"><voice name="en-US-JennyNeural"><sub alias="kitten">cat</sub></voice></speak> "#;
    for _ in 0..2 {
        service
            .create_manual_preview(
                actor,
                ManualPreviewRequest {
                    voice_alias: "en-us-jenny".into(),
                    ssml: raw.into(),
                    output_format: AudioOutputFormat::default(),
                },
            )
            .await
            .unwrap();
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert!(
        service
            .create_manual_preview(
                actor,
                ManualPreviewRequest {
                    voice_alias: "en-us-jenny".into(),
                    ssml: raw.replace(
                        "<sub alias=\"kitten\">cat</sub>",
                        "<audio src=\"https://example.com/a\"/>"
                    ),
                    output_format: AudioOutputFormat::default()
                }
            )
            .await
            .is_err()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='speech.generate'").bind(actor).execute(&pool).await.unwrap();
    assert!(service.read_content(actor, &hash).await.is_err());
}

#[sqlx::test]
async fn voice_listing_hides_provider_identity_and_disabled_rows(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    insert_voice(&pool).await;
    sqlx::query("UPDATE speech.voices SET provider_voice_id = 'en-US-AvaNeural' WHERE alias = 'en-us-jenny'")
        .execute(&pool).await.unwrap();
    sqlx::query(
        r#"INSERT INTO speech.voices
           (id, alias, provider, provider_voice_id, locale, gender, provider_version, enabled)
           VALUES ($1, 'disabled', 'azure', 'secret-provider-id', 'en-US', 'male', 'v1', false)"#,
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
    let service = PreviewService::new(
        pool.clone(),
        PreviewRepository::new(pool),
        redis_pool(),
        None,
        None,
    );
    let response = service.list_voices(admin_id).await.unwrap();
    assert_eq!(response.items.len(), 1);
    assert_eq!(response.items[0].alias, "en-us-jenny");
    assert_eq!(response.items[0].capabilities.styles, vec!["chat"]);
}

#[sqlx::test]
async fn voice_catalog_allows_sentences_only_but_generation_still_requires_grant(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id = $1")
        .bind(admin_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_permission_grants (admin_id, permission_key, granted_by) VALUES ($1, 'sentences.access', $1)")
        .bind(admin_id).execute(&pool).await.unwrap();
    insert_voice(&pool).await;
    let service = PreviewService::new(
        pool.clone(),
        PreviewRepository::new(pool.clone()),
        redis_pool(),
        None,
        None,
    );
    let voices = service.list_voices(admin_id).await.unwrap();
    assert_eq!(voices.items.len(), 1);
    let denied = service
        .create_preview(admin_id, request("sentences-only"))
        .await
        .unwrap_err();
    assert!(
        matches!(denied, tsz_rust::speech::preview::PreviewServiceError::Authorization(ref error) if error.status_code() == axum::http::StatusCode::FORBIDDEN)
    );
    sqlx::query("INSERT INTO admin_permission_grants (admin_id, permission_key, granted_by) VALUES ($1, 'words.access', $1)")
        .bind(admin_id).execute(&pool).await.unwrap();
    let denied = service
        .create_preview(admin_id, request("without-generation-grant"))
        .await
        .unwrap_err();
    assert!(
        matches!(denied, tsz_rust::speech::preview::PreviewServiceError::Authorization(ref error) if error.status_code() == axum::http::StatusCode::FORBIDDEN)
    );
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id = $1")
        .bind(admin_id)
        .execute(&pool)
        .await
        .unwrap();
    let denied = service.list_voices(admin_id).await.unwrap_err();
    assert!(
        matches!(denied, tsz_rust::speech::preview::PreviewServiceError::Authorization(ref error) if error.status_code() == axum::http::StatusCode::FORBIDDEN)
    );
}

#[sqlx::test]
async fn concurrent_same_fingerprint_has_one_provider_owner(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    insert_voice(&pool).await;
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    let store: Arc<dyn ObjectStore> = MemoryAdapter::object_store(
        StorageSpace::parse("speech").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    );
    let service = PreviewService::new(
        pool.clone(),
        PreviewRepository::new(pool),
        redis_pool(),
        Some(provider.clone()),
        Some(store),
    );
    let first = service.clone();
    let second = service;
    let (left, right) = tokio::join!(
        async move {
            first
                .create_preview(admin_id, request("concurrent-test"))
                .await
        },
        async move {
            second
                .create_preview(admin_id, request("concurrent-test"))
                .await
        },
    );
    assert!(left.is_ok(), "first request should finish");
    assert!(right.is_ok(), "second request should hit winner");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}
