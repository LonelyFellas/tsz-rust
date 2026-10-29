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

#[path = "speech_generated/audio_updates.rs"]
mod audio_updates_tests;
#[path = "speech_generated/publication_review.rs"]
mod publication_review;

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
async fn same_effect_new_request_reuses_bytes_in_a_distinct_owned_asset(pool: PgPool) {
    let actor = actor(&pool).await;
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: false,
    });
    let storage = store();
    let service =
        GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(storage.clone()));
    let first = service
        .generate(actor, 0, input(Uuid::now_v7(), Uuid::now_v7(), "hello"))
        .await
        .unwrap()
        .asset;
    let second = service
        .generate(actor, 0, input(Uuid::now_v7(), Uuid::now_v7(), "hello"))
        .await
        .unwrap()
        .asset;
    assert_ne!(
        first.id, second.id,
        "business assets must retain independent ownership"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "same synthesis effect must not be charged twice"
    );
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT object_key FROM lexicon.audio_assets WHERE id=ANY($1) ORDER BY id",
    )
    .bind(vec![first.id, second.id])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_ne!(keys[0], keys[1]);
    for key in keys {
        assert_eq!(
            storage.read(&ObjectKey::parse(key).unwrap()).await.unwrap(),
            b"ID3-test-audio"
        );
    }
    assert_eq!(second.size_bytes, first.size_bytes);
}

struct WavProvider;

#[sqlx::test]
async fn reuse_preserves_explicit_regeneration_and_effect_changes(pool: PgPool) {
    let actor = actor(&pool).await;
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: false,
    });
    let service = GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(store()));
    let first = service
        .generate(actor, 0, input(Uuid::now_v7(), Uuid::now_v7(), "hello"))
        .await
        .unwrap()
        .asset;
    let mut regenerate = input(Uuid::now_v7(), Uuid::now_v7(), "hello");
    regenerate.setting.audio_asset_id = Some(first.id);
    let request = regenerate.request_id;
    let version = regenerate.setting.version_id.unwrap();
    let second = service.generate(actor, 0, regenerate).await.unwrap().asset;
    assert_ne!(first.id, second.id);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let mut retry = input(request, version, "hello");
    retry.setting.audio_asset_id = Some(first.id);
    assert_eq!(
        service.generate(actor, 0, retry).await.unwrap().asset.id,
        second.id
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

    let mut faster = input(Uuid::now_v7(), Uuid::now_v7(), "hello");
    faster.setting.rate_percent = 10;
    service.generate(actor, 0, faster).await.unwrap();
    service
        .generate(actor, 0, input(Uuid::now_v7(), Uuid::now_v7(), "world"))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    sqlx::query("UPDATE speech.voices SET provider_version='v2' WHERE alias='sonia'")
        .execute(&pool)
        .await
        .unwrap();
    service
        .generate(actor, 0, input(Uuid::now_v7(), Uuid::now_v7(), "hello"))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 5);
}

#[sqlx::test]
async fn reuse_is_owner_scoped_and_excludes_reclaiming_assets(pool: PgPool) {
    let owner = actor(&pool).await;
    let other = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id: other,
            phone: format!("reuse-{other}"),
            display_name: "Other editor".into(),
            password_hash: "hash".into(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) SELECT $1,unnest(ARRAY['words.access','words.edit','speech.generate']),$1").bind(other).execute(&pool).await.unwrap();
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: false,
    });
    let service = GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(store()));
    let first = service
        .generate(owner, 0, input(Uuid::now_v7(), Uuid::now_v7(), "hello"))
        .await
        .unwrap()
        .asset;
    let second = service
        .generate(other, 0, input(Uuid::now_v7(), Uuid::now_v7(), "hello"))
        .await
        .unwrap()
        .asset;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let second_owner: Uuid =
        sqlx::query_scalar("SELECT created_by_admin_id FROM lexicon.audio_assets WHERE id=$1")
            .bind(second.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(second_owner, other);
    sqlx::query("UPDATE lexicon.audio_assets SET reclamation_started_at=now() WHERE id=$1")
        .bind(first.id)
        .execute(&pool)
        .await
        .unwrap();
    service
        .generate(owner, 0, input(Uuid::now_v7(), Uuid::now_v7(), "hello"))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
}

#[sqlx::test]
async fn unreadable_reuse_source_fails_without_spending_and_can_retry(pool: PgPool) {
    let actor = actor(&pool).await;
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: false,
    });
    let storage = store();
    let service =
        GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(storage.clone()));
    let first = service
        .generate(actor, 0, input(Uuid::now_v7(), Uuid::now_v7(), "hello"))
        .await
        .unwrap()
        .asset;
    let key: String = sqlx::query_scalar("SELECT object_key FROM lexicon.audio_assets WHERE id=$1")
        .bind(first.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let key = ObjectKey::parse(key).unwrap();
    storage.delete(&key).await.unwrap();
    let request = Uuid::now_v7();
    let version = Uuid::now_v7();
    assert!(
        service
            .generate(actor, 0, input(request, version, "hello"))
            .await
            .is_err()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let state: String =
        sqlx::query_scalar("SELECT state FROM lexicon.audio_generations WHERE request_id=$1")
            .bind(request)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    storage
        .put(
            &key,
            b"ID3-test-audio".to_vec(),
            tsz_rust::platform::storage::PutOptions::new(Some(
                tsz_rust::platform::storage::ObjectContentType::parse("audio/mpeg").unwrap(),
            )),
        )
        .await
        .unwrap();
    service
        .generate(actor, 0, input(request, version, "hello"))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[sqlx::test]
async fn reuse_index_migration_preserves_generated_assets(pool: PgPool) {
    let actor = actor(&pool).await;
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: false,
    });
    let service = GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(store()));
    let request = Uuid::now_v7();
    let version = Uuid::now_v7();
    let first = service
        .generate(actor, 0, input(request, version, "hello"))
        .await
        .unwrap()
        .asset;
    sqlx::raw_sql(include_str!(
        "../migrations/20261028180000_generated_audio_reuse.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261028180000_generated_audio_reuse.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        service
            .generate(actor, 0, input(request, version, "hello"))
            .await
            .unwrap()
            .asset
            .id,
        first.id
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[async_trait]
impl SpeechProvider for WavProvider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }
    async fn synthesize(
        &self,
        request: &SynthesisRequest,
    ) -> Result<SynthesizedAudio, SpeechError> {
        assert_eq!(
            request.options().output_format(),
            tsz_rust::speech::AudioOutputFormat::Wav24Khz16BitMono
        );
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
        Ok(SynthesizedAudio {
            bytes,
            content_type: "audio/wav",
            provider_request_id: None,
        })
    }
}

#[sqlx::test]
async fn wav_generation_records_actual_format_and_does_not_reuse_mp3_identity(pool: PgPool) {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let actor = actor(&pool).await;
    let storage = store();
    let service = GeneratedAudioService::new(
        pool.clone(),
        Some(Arc::new(WavProvider)),
        Some(storage.clone()),
    );
    let mut request = input(Uuid::now_v7(), Uuid::now_v7(), "hello");
    request.setting.output_format = Some(tsz_rust::speech::AudioOutputFormat::Wav24Khz16BitMono);
    let request_id = request.request_id;
    let version = request.setting.version_id.unwrap();
    let asset = service.generate(actor, 0, request).await.unwrap().asset;
    assert_eq!(asset.content_type, "audio/wav");
    assert!(asset.original_name.ends_with(".wav"));
    let mut state = tsz_rust::state::AppState::for_test(pool.clone());
    state.object_storage =
        tsz_rust::platform::storage::StorageRegistry::from_stores([storage]).unwrap();
    let token = state.admin_token_manager.generate(actor, "admin").unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/admin/lexicon/audio-assets/{}/content",
                    asset.id
                ))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/wav");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(
        service
            .generate(actor, 0, input(request_id, version, "hello"))
            .await
            .unwrap_err()
            .code(),
        tsz_rust::error::ErrorCode::IdempotencyConflict
    );
    let stranger = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id: stranger,
            phone: format!("wav-reader-{stranger}"),
            display_name: "reader".into(),
            password_hash: "hash".into(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.access',$1)").bind(stranger).execute(&pool).await.unwrap();
    let token = state
        .admin_token_manager
        .generate(stranger, "admin")
        .unwrap();
    let response = tsz_rust::router(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/admin/lexicon/audio-assets/{}/content",
                    asset.id
                ))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn inherited_generation_rejects_stale_recommendations_before_provider_and_reparses_capabilities(
    pool: PgPool,
) {
    let actor = actor(&pool).await;
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: false,
    });
    let service = GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(store()));
    let version = Uuid::now_v7();
    let groups = json!([{"id":Uuid::now_v7(),"name":"word","areas":["word"],"revision":1,"is_global":false,
        "uk":[{"id":version,"name":"natural","voice_alias":"sonia","selected":true,"parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}}],"us":[]}]);
    sqlx::query("UPDATE speech.dubbing_groups SET groups=$1 WHERE singleton")
        .bind(groups)
        .execute(&pool)
        .await
        .unwrap();
    let mut request = input(Uuid::now_v7(), version, "123");
    request.profile = Some(
        serde_json::from_value(json!({"voices":[],"dubbing":{"mode":"inherit","area":"word"}}))
            .unwrap(),
    );
    service.generate(actor, 0, request).await.unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    sqlx::query("UPDATE speech.dubbing_groups SET groups=jsonb_set(groups,'{0,uk,0,parameters,rate_percent}','-27') WHERE singleton").execute(&pool).await.unwrap();
    let mut stale = input(Uuid::now_v7(), version, "123");
    stale.profile = Some(
        serde_json::from_value(json!({"voices":[],"dubbing":{"mode":"inherit","area":"word"}}))
            .unwrap(),
    );
    assert_eq!(
        service.generate(actor, 0, stale).await.unwrap_err().code(),
        tsz_rust::error::ErrorCode::RevisionConflict
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let mut conn = pool.acquire().await.unwrap();
    let mut content: tsz_rust::lexicon::dto::RichTextV2 = serde_json::from_value(json!({"version":2,"text":"123","annotations":[{"type":"say_as","start":0,"end":3,"interpret_as":"digits"}]})).unwrap();
    let profile =
        serde_json::from_value(json!({"voices":[],"dubbing":{"mode":"inherit","area":"word"}}))
            .unwrap();
    let resolved = tsz_rust::speech::dubbing::resolve_in(&mut conn, &profile, "gb", &content, None)
        .await
        .unwrap();
    assert_eq!(resolved[0].rate_percent, -27);
    content.annotations = vec![
        serde_json::from_value(json!({"type":"sub","start":0,"end":3,"alias":"one two three"}))
            .unwrap(),
    ];
    assert!(
        tsz_rust::speech::dubbing::resolve_in(&mut conn, &profile, "gb", &content, None)
            .await
            .is_ok()
    );
}

#[sqlx::test]
async fn inheritance_dependencies_are_removed_transactionally_when_switching_to_local(
    pool: PgPool,
) {
    let owner = Uuid::now_v7();
    let target = Uuid::now_v7();
    let inherited = json!({"id":target,"voice_profile":{"voices":[],"dubbing":{"mode":"inherit","area":"word"}}});
    let local =
        json!({"id":target,"voice_profile":{"voices":[],"dubbing":{"mode":"local","area":"word"}}});
    let mut tx = pool.begin().await.unwrap();
    tsz_rust::speech::dubbing::replace_dependencies(&mut tx, "entry", owner, 1, &inherited)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let count = || {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM speech.dubbing_dependencies WHERE owner_id=$1",
        )
        .bind(owner)
    };
    assert_eq!(count().fetch_one(&pool).await.unwrap(), 1);
    let mut tx = pool.begin().await.unwrap();
    tsz_rust::speech::dubbing::replace_dependencies(&mut tx, "entry", owner, 2, &local)
        .await
        .unwrap();
    assert_eq!(count().fetch_one(&mut *tx).await.unwrap(), 0);
    tx.rollback().await.unwrap();
    assert_eq!(count().fetch_one(&pool).await.unwrap(), 1);
    let mut tx = pool.begin().await.unwrap();
    tsz_rust::speech::dubbing::replace_dependencies(&mut tx, "entry", owner, 2, &local)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(count().fetch_one(&pool).await.unwrap(), 0);
}

#[sqlx::test]
async fn dubbing_migration_roundtrips_empty_schema_but_rejects_saved_local_semantics(pool: PgPool) {
    let down = include_str!("../migrations/20261026110000_dubbing_dependencies.down.sql");
    let up = include_str!("../migrations/20261026110000_dubbing_dependencies.up.sql");
    sqlx::raw_sql(down).execute(&pool).await.unwrap();
    sqlx::raw_sql(up).execute(&pool).await.unwrap();
    let actor = actor(&pool).await;
    let entry = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,revision,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word',1,'{}',$2,$2)").bind(entry).bind(actor).execute(&pool).await.unwrap();
    let forms = json!({"pos":[],"saved":{"voice_profile":{"voices":[],"dubbing":{"mode":"local","area":"word"}}}});
    sqlx::query("INSERT INTO lexicon.entry_editor_projection(entry_id,forms,meanings,rebuilt_revision) VALUES($1,$2,'{\"pos\":[],\"sense_groups\":[]}',1)").bind(entry).bind(forms).execute(&pool).await.unwrap();
    assert!(
        sqlx::raw_sql(down)
            .execute(&pool)
            .await
            .unwrap_err()
            .to_string()
            .contains("Cannot roll back")
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT to_regclass('speech.dubbing_dependencies') IS NOT NULL"
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    );
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
    sqlx::query("UPDATE speech.voices SET enabled=false WHERE alias='sonia'")
        .execute(&pool)
        .await
        .unwrap();
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
async fn phoneme_special_reading_overrides_only_the_explicit_word_range(pool: PgPool) {
    let actor = actor(&pool).await;
    let service = GeneratedAudioService::new(
        pool.clone(),
        Some(Arc::new(Provider {
            calls: AtomicUsize::new(0),
            fail_first: false,
        })),
        Some(store()),
    );
    let mut request = input(Uuid::now_v7(), Uuid::now_v7(), "far away");
    request.source = serde_json::from_value(json!({"kind":"phoneme","content":{"version":2,"text":"far away","annotations":[{"type":"sub","start":0,"end":3,"alias":"distant"}]},"alphabet":"ups","phoneme":"F AA lng AX W EY","ups_words":[{"text":"far","phoneme":"F AA lng"},{"text":"away","phoneme":"AX W EY"}]})).unwrap();
    let asset = service.generate(actor, 0, request).await.unwrap().asset;
    let resolved: serde_json::Value = sqlx::query_scalar(
        "SELECT resolved_content FROM lexicon.audio_generations WHERE asset_id=$1",
    )
    .bind(asset.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let marks = resolved["annotations"].as_array().unwrap();
    assert!(
        marks
            .iter()
            .any(|mark| mark["type"] == "sub" && mark["alias"] == "distant")
    );
    assert_eq!(
        marks
            .iter()
            .filter(|mark| mark["type"] == "phoneme")
            .count(),
        1
    );
    assert!(
        marks
            .iter()
            .any(|mark| mark["type"] == "phoneme" && mark["start"] == 4)
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
            .bind(Uuid::now_v7()).bind(&alias).bind(tsz_rust::speech::capabilities::snapshot().items.iter().find(|item| item.locale == locale && item.gender == "female" && item.model == "neural").unwrap().provider_voice_id.clone()).bind(locale).execute(&pool).await.unwrap();
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
