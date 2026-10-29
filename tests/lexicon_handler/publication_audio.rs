//! Explicit publication fixture prerequisites. Raw request helpers never repair
//! content or synthesize audio, so rejection and concurrency tests remain raw.
use super::*;
use std::time::Duration;
use tsz_rust::platform::storage::{
    MemoryAdapter, ObjectContentType, ObjectKey, PutOptions, StoragePolicy, StoragePrivacy,
    StorageRegistry, StorageSpace,
};

pub(super) fn fixture_state(pool: PgPool, redis: deadpool_redis::Pool) -> AppState {
    let mut state = AppState::for_test_with_redis(pool, redis);
    let store = MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    );
    state.object_storage = StorageRegistry::from_stores([store]).unwrap();
    state
}

/// Add real, readable recording fixtures before the initial forms save, without
/// changing draft revisions or disabling any selected synthesis configuration.
pub(super) async fn audio_forms(state: &AppState, entry_id: &str, mut forms: Value) -> Value {
    let actor: Uuid =
        sqlx::query_scalar("SELECT created_by_admin_id FROM lexicon.entries WHERE id=$1")
            .bind(Uuid::parse_str(entry_id).unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let store = state
        .object_storage
        .get(&StorageSpace::parse("audio").unwrap())
        .unwrap();
    let mut assets = Vec::new();
    for locale in ["en-GB", "en-US"] {
        let id = Uuid::now_v7();
        let key = format!("fixtures/publication/{entry_id}/{id}.mp3");
        store
            .put(
                &ObjectKey::parse(&key).unwrap(),
                b"ID3".to_vec(),
                PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
            )
            .await
            .unwrap();
        let asset: Value = sqlx::query_scalar("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id) VALUES($1,$2,$2,'audio/mpeg',3,$3,'female','fixture.mp3',$4) RETURNING jsonb_build_object('id',id,'content_type',content_type,'size_bytes',size_bytes,'locale',locale,'gender',gender,'original_name',original_name,'duration_ms',duration_ms,'created_at',created_at)")
            .bind(id).bind(key).bind(locale).bind(actor).fetch_one(&state.pool).await.unwrap();
        assets.push(asset);
    }
    fn attach(value: &mut Value, assets: &[Value]) {
        if value.get("spelling").is_some() && value["pronunciations"].is_array() {
            let dialect = value["dialect"].as_str().unwrap_or("common").to_owned();
            for pronunciation in value["pronunciations"].as_array_mut().unwrap() {
                let recordings = pronunciation
                    .as_object_mut()
                    .unwrap()
                    .entry("audio_assets")
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .unwrap();
                for asset in assets {
                    let matches_accent = dialect == "common"
                        || (dialect == "uk" && asset["locale"] == "en-GB")
                        || (dialect == "us" && asset["locale"] == "en-US");
                    if matches_accent
                        && !recordings
                            .iter()
                            .any(|existing| existing["locale"] == asset["locale"])
                    {
                        recordings.push(asset.clone());
                    }
                }
            }
            return;
        }
        match value {
            Value::Object(fields) => {
                for child in fields.values_mut() {
                    attach(child, assets);
                }
            }
            Value::Array(items) => {
                for child in items {
                    attach(child, assets);
                }
            }
            _ => {}
        }
    }
    attach(&mut forms, &assets);
    forms
}

pub(super) async fn confirm_audio(state: &AppState, bearer: &str, word: &Value) {
    let id = word["word"]["id"].as_str().unwrap();
    let revision = &word["word"]["revision"];
    let (status, review) = call(
        state,
        Method::POST,
        &format!("{ROOT}/entries/{id}/publication-audio/check"),
        bearer,
        None,
        Some(json!({"base_revision":revision})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{review}");
    assert_eq!(review["manifest"]["ready"], true, "{review}");
    let (status, confirmed) = call(
        state,
        Method::POST,
        &format!("{ROOT}/entries/{id}/publication-audio/confirm"),
        bearer,
        None,
        Some(json!({"base_revision":revision,"manifest_hash":review["manifest"]["manifest_hash"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmed}");
    assert_eq!(confirmed["confirmed"], true, "{confirmed}");
}

pub(super) async fn publish_reviewed_v3(
    state: &AppState,
    bearer: &str,
    word: &Value,
) -> (StatusCode, Value) {
    confirm_audio(state, bearer, word).await;
    publish_ready_v3(state, bearer, word).await
}

pub(super) async fn generated_profile(
    state: &AppState,
    actor: Uuid,
    mut profile: Value,
    content: Value,
) -> Value {
    use tsz_rust::speech::{
        SpeechError, SpeechProvider, SynthesisRequest, SynthesizedAudio,
        generated::{GenerateVoiceAudioRequest, GeneratedAudioService},
    };
    struct Provider;
    #[async_trait::async_trait]
    impl SpeechProvider for Provider {
        fn provider_name(&self) -> &'static str {
            "azure"
        }
        async fn synthesize(&self, _: &SynthesisRequest) -> Result<SynthesizedAudio, SpeechError> {
            Ok(SynthesizedAudio {
                bytes: b"ID3".to_vec(),
                content_type: "audio/mpeg",
                provider_request_id: None,
            })
        }
    }
    for (alias, provider, locale, styles) in [
        ("sonia", "en-GB-SoniaNeural", "en-GB", json!([])),
        ("jenny", "en-US-JennyNeural", "en-US", json!(["cheerful"])),
    ] {
        sqlx::query("INSERT INTO speech.voices(id,alias,provider,provider_voice_id,locale,gender,styles,provider_version) VALUES($1,$2,'azure',$3,$4,'female',$5,'v1') ON CONFLICT(alias) DO NOTHING")
            .bind(Uuid::now_v7()).bind(alias).bind(provider).bind(locale).bind(styles).execute(&state.pool).await.unwrap();
    }
    let service = GeneratedAudioService::new(
        state.pool.clone(),
        Some(Arc::new(Provider)),
        Some(
            state
                .object_storage
                .get(&StorageSpace::parse("audio").unwrap())
                .unwrap(),
        ),
    );
    for setting in profile["voices"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .filter(|v| v["enabled"] == true)
    {
        setting["version_id"] = setting
            .get("version_id")
            .cloned()
            .unwrap_or_else(|| json!(Uuid::now_v7()));
        if setting.get("version_name").is_none() {
            setting["version_name"] = json!("fixture");
        }
        if setting.get("locale").is_none() {
            setting["locale"] = json!(if setting["voice_id"] == "jenny" {
                "en-US"
            } else {
                "en-GB"
            });
        }
        let input:GenerateVoiceAudioRequest=serde_json::from_value(json!({"request_id":Uuid::now_v7(),"source":{"kind":"text","content":content},"setting":setting})).unwrap();
        setting["audio_asset_id"] =
            json!(service.generate(actor, 0, input).await.unwrap().asset.id);
    }
    profile
}

pub(super) async fn save_publishable_forms(
    state: &AppState,
    bearer: &str,
    id: &str,
    revision: i64,
    intent: &str,
    forms: Value,
) -> (Value, Value) {
    let forms = audio_forms(state, id, forms).await;
    save_v3_forms_after_impact(state, bearer, id, revision, intent, forms).await
}

tokio::task_local! { static GENERATED_CONTRACT: (); }
pub(super) fn generated_contract() -> bool {
    GENERATED_CONTRACT.try_with(|_| ()).is_ok()
}
pub(super) async fn with_generated_contract<F: std::future::Future<Output = ()>>(body: F) {
    GENERATED_CONTRACT.scope((), body).await;
}
