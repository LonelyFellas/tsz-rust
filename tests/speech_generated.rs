use async_trait::async_trait;
use serde_json::json;
use sqlx::PgPool;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    platform::storage::{
        MemoryAdapter, ObjectKey, ObjectStore, StoragePolicy, StoragePrivacy, StorageSpace,
    },
    speech::{
        SpeechError, SpeechErrorKind, SpeechProvider, SynthesisRequest, SynthesizedAudio,
        generated::{GenerateVoiceAudioRequest, GeneratedAudioService},
    },
};
use uuid::Uuid;

struct Provider {
    calls: AtomicUsize,
    fail_first: bool,
}
#[async_trait]
impl SpeechProvider for Provider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }
    async fn synthesize(&self, _: &SynthesisRequest) -> Result<SynthesizedAudio, SpeechError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_first && call == 0 {
            return Err(SpeechError::new(SpeechErrorKind::Unavailable, None));
        }
        Ok(SynthesizedAudio {
            bytes: b"ID3-test-audio".to_vec(),
            content_type: "audio/mpeg",
            provider_request_id: None,
        })
    }
}
async fn actor(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("generated-{id}"),
            display_name: "Generated audio test".into(),
            password_hash: "hash".into(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) SELECT $1,unnest(ARRAY['words.access','words.edit','speech.generate']),$1").bind(id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO speech.voices(id,alias,provider,provider_voice_id,locale,gender,styles,provider_version) VALUES($1,'sonia','azure','en-GB-SoniaNeural','en-GB','female','[]','v1')").bind(Uuid::now_v7()).execute(pool).await.unwrap();
    id
}
fn input(request: Uuid, version: Uuid, text: &str) -> GenerateVoiceAudioRequest {
    serde_json::from_value(json!({"request_id":request,"source":{"kind":"text","content":{"version":2,"text":text,"annotations":[]}},"setting":{"version_id":version,"version_name":"自然","locale":"en-GB","voice_id":"sonia","enabled":true,"rate_percent":0}})).unwrap()
}
fn store() -> Arc<dyn ObjectStore> {
    MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    )
}

#[sqlx::test]
async fn generated_asset_is_long_lived_idempotent_and_retries_failed_generation(pool: PgPool) {
    let actor = actor(&pool).await;
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: true,
    });
    let storage = store();
    let service =
        GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(storage.clone()));
    let request = Uuid::now_v7();
    let version = Uuid::now_v7();
    assert!(
        service
            .generate(actor, 0, input(request, version, "hello"))
            .await
            .is_err()
    );
    let asset = service
        .generate(actor, 0, input(request, version, "hello"))
        .await
        .unwrap()
        .asset;
    let repeated = service
        .generate(actor, 0, input(request, version, "hello"))
        .await
        .unwrap()
        .asset;
    assert_eq!(asset.id, repeated.id);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let key: String = sqlx::query_scalar("SELECT object_key FROM lexicon.audio_assets WHERE id=$1")
        .bind(asset.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(key.starts_with("assets/generated/"));
    assert_eq!(
        storage.read(&ObjectKey::parse(key).unwrap()).await.unwrap(),
        b"ID3-test-audio"
    );
    assert_eq!(asset.size_bytes, 14);
    assert!(
        service
            .generate(actor, 0, input(request, version, "changed"))
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='speech.generate'").bind(actor).execute(&pool).await.unwrap();
    assert!(
        service
            .generate(actor, 0, input(request, version, "hello"))
            .await
            .is_err()
    );
}

#[sqlx::test]
async fn phoneme_generation_uses_spelling_and_word_pauses(pool: PgPool) {
    let actor = actor(&pool).await;
    let service = GeneratedAudioService::new(
        pool.clone(),
        Some(Arc::new(Provider {
            calls: AtomicUsize::new(0),
            fail_first: false,
        })),
        Some(store()),
    );
    let mut request = input(Uuid::now_v7(), Uuid::now_v7(), "hello world");
    request.source=serde_json::from_value(json!({"kind":"phoneme","content":{"version":2,"text":"hello world","annotations":[{"type":"pause","at":5,"duration_ms":500}]},"alphabet":"ups","phoneme":"HH AH L OW W ER L D","ups_words":[{"text":"hello","phoneme":"HH AH L OW"},{"text":"world","phoneme":"W ER L D"}]})).unwrap();
    let asset = service.generate(actor, 0, request).await.unwrap().asset;
    let resolved: serde_json::Value = sqlx::query_scalar(
        "SELECT resolved_content FROM lexicon.audio_generations WHERE asset_id=$1",
    )
    .bind(asset.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(resolved["text"], "hello world");
    assert_eq!(
        resolved["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "phoneme")
            .count(),
        2
    );
    assert!(
        resolved["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "pause" && item["at"] == 5)
    );
}

#[sqlx::test]
async fn generation_migration_can_be_reverted_without_deleting_assets(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../migrations/20261025140000_generated_voice_audio.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let missing: bool =
        sqlx::query_scalar("SELECT to_regclass('lexicon.audio_generations') IS NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(missing);
    let assets: bool = sqlx::query_scalar("SELECT to_regclass('lexicon.audio_assets') IS NOT NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(assets);
    sqlx::raw_sql(include_str!(
        "../migrations/20261025140000_generated_voice_audio.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn native_regions_generate_assets_and_reject_phonemes_before_provider_call(pool: PgPool) {
    let actor = actor(&pool).await;
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: false,
    });
    let service = GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(store()));
    for locale in ["en-AU", "en-NZ", "en-CA", "en-IN"] {
        let alias = locale.to_ascii_lowercase();
        sqlx::query("INSERT INTO speech.voices(id,alias,provider,provider_voice_id,locale,gender,styles,provider_version) VALUES($1,$2,'azure',$3,$4,'female','[]','v1')")
            .bind(Uuid::now_v7()).bind(&alias).bind(format!("{locale}-TestNeural")).bind(locale).execute(&pool).await.unwrap();
        let mut request = input(Uuid::now_v7(), Uuid::now_v7(), "hello");
        request.setting.voice_id = alias;
        request.setting.locale = tsz_rust::speech::locale::EnglishLocale::parse(locale);
        let asset = service.generate(actor, 0, request).await.unwrap().asset;
        assert_eq!(asset.locale.as_str(), locale);
        let stored: String =
            sqlx::query_scalar("SELECT locale FROM lexicon.audio_assets WHERE id=$1")
                .bind(asset.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, locale);
        let mut bad = input(Uuid::now_v7(), Uuid::now_v7(), "hello");
        bad.setting.voice_id = locale.to_ascii_lowercase();
        bad.setting.locale = tsz_rust::speech::locale::EnglishLocale::parse(locale);
        bad.source = serde_json::from_value(json!({"kind":"phoneme","content":{"version":2,"text":"hello","annotations":[]},"alphabet":"ipa","phoneme":"həˈləʊ","phoneme_locale":"en-GB"})).unwrap();
        let before = provider.calls.load(Ordering::SeqCst);
        assert!(service.generate(actor, 0, bad).await.is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), before);
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
}

#[sqlx::test]
async fn flash_generates_only_natural_defaults_and_records_provider_identity(pool: PgPool) {
    let actor = actor(&pool).await;
    let voice = tsz_rust::speech::Voice::new(
        "azure",
        "en-US-Tiana:DragonHDFlashLatestNeural",
        "en-US",
        [],
    )
    .unwrap();
    let catalog = tsz_rust::speech::CatalogVoice {
        voice,
        gender: "female".into(),
    };
    tsz_rust::speech::preview::PreviewRepository::new(pool.clone())
        .sync_voices(std::slice::from_ref(&catalog))
        .await
        .unwrap();
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: false,
    });
    let service = GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(store()));
    let mut request = input(Uuid::now_v7(), Uuid::now_v7(), "hello");
    request.setting.voice_id = catalog.alias();
    request.setting.locale = tsz_rust::speech::locale::EnglishLocale::parse("en-US");
    let asset = service.generate(actor, 0, request).await.unwrap().asset;
    let identity: serde_json::Value =
        sqlx::query_scalar("SELECT voice FROM lexicon.audio_generations WHERE asset_id=$1")
            .bind(asset.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        identity["provider_voice_id"],
        "en-US-Tiana:DragonHDFlashLatestNeural"
    );
    assert_eq!(identity["builder_version"], "hd-flash-natural-ssml-v1");
    for (rate, volume, pause) in [(1, 100, false), (0, 90, false), (0, 100, true)] {
        let mut request = input(Uuid::now_v7(), Uuid::now_v7(), "hello");
        request.setting.voice_id = catalog.alias();
        request.setting.locale = tsz_rust::speech::locale::EnglishLocale::parse("en-US");
        request.setting.rate_percent = rate;
        request.setting.volume_percent = Some(volume);
        if pause {
            request.source=serde_json::from_value(json!({"kind":"text","content":{"version":2,"text":"hello","annotations":[{"type":"pause","at":5,"duration_ms":300}]}})).unwrap();
        }
        assert!(service.generate(actor, 0, request).await.is_err());
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[sqlx::test]
async fn generation_requires_content_edit_permission_but_not_upload_permission(pool: PgPool) {
    let actor = actor(&pool).await;
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: false,
    });
    let service = GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(store()));
    sqlx::query(
        "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='words.edit'",
    )
    .bind(actor)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        service
            .generate(actor, 0, input(Uuid::now_v7(), Uuid::now_v7(), "hello"))
            .await
            .is_err()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}
