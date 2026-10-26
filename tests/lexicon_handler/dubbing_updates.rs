use super::*;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tsz_rust::{
    platform::storage::{MemoryAdapter, StoragePolicy, StoragePrivacy, StorageSpace},
    speech::{SpeechError, SpeechProvider, SynthesisRequest, SynthesizedAudio, audio_updates},
};

struct Provider(AtomicUsize);
#[async_trait::async_trait]
impl SpeechProvider for Provider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }
    async fn synthesize(
        &self,
        request: &SynthesisRequest,
    ) -> Result<SynthesizedAudio, SpeechError> {
        let ssml = tsz_rust::speech::build_ssml(request).unwrap();
        assert!(ssml.contains("harbour") || ssml.contains("harbor"));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(SynthesizedAudio {
            bytes: b"ID3-fixture".to_vec(),
            content_type: "audio/mpeg",
            provider_request_id: None,
        })
    }
}

#[sqlx::test]
async fn inherited_word_updates_persist_shared_spelling_ipa_ups_and_asset_references(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let created = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = created["word"]["id"].as_str().unwrap();
    let entry = Uuid::parse_str(id).unwrap();
    for (alias, provider, locale) in [
        ("sonia", "en-GB-SoniaNeural", "en-GB"),
        ("jenny", "en-US-JennyNeural", "en-US"),
    ] {
        sqlx::query("INSERT INTO speech.voices(id,alias,provider,provider_voice_id,locale,gender,styles,provider_version) VALUES($1,$2,'azure',$3,$4,'female','[]','v1')").bind(Uuid::now_v7()).bind(alias).bind(provider).bind(locale).execute(&pool).await.unwrap();
    }
    async fn latest(state: &AppState, bearer: &str, id: &str) -> Value {
        let request = Request::builder()
            .uri(format!("{ROOT}/entries/{id}"))
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header("X-TSZ-Form-Contents", "v1")
            .header("X-TSZ-Voice-Versions", "v3")
            .header("X-TSZ-Voice-Locales", "v1")
            .header("X-TSZ-Voice-Models", "v1")
            .header("X-TSZ-Spelling-Speech", "v1")
            .header("X-TSZ-Phoneme-Speech", "v1")
            .body(Body::empty())
            .unwrap();
        let response = tsz_rust::router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }
    let read = latest(&state, &bearer, id).await;
    let mut forms = read["word"]["forms"].clone();
    let content_id = Uuid::parse_str(forms["contents"][0]["id"].as_str().unwrap()).unwrap();
    let path = "/contents/0/regional_variants/common";
    let (path, locale) = if forms.pointer(path).is_some() {
        (path, "uk")
    } else {
        ("/contents/0/regional_variants/uk", "uk")
    };
    let variant = forms.pointer_mut(path).unwrap();
    let spelling = variant["spelling"].clone();
    let profile = |area| json!({"voices":[],"dubbing":{"mode":"inherit","area":area}});
    let speech = |area| json!({"content":{"version":2,"text":spelling,"annotations":[]},"voice_profile":profile(area)});
    variant["spelling_speech"] = speech("word");
    variant["pronunciations"][0]["synthesis"] = json!({"alphabet":"ipa","ipa":"","ups":"",locale:{"ipa":"ˈhɑːbə","ups":"HH AA R B AX R","ipa_speech":speech("azure"),"ups_speech":speech("azure")}});
    let request=Request::builder().method(Method::PUT).uri(format!("{ROOT}/entries/{id}/steps/forms")).header(header::AUTHORIZATION,format!("Bearer {bearer}")).header(header::CONTENT_TYPE,"application/json").header("X-TSZ-Form-Contents","v1").header("X-TSZ-Voice-Versions","v3").header("X-TSZ-Voice-Locales","v1").header("X-TSZ-Voice-Models","v1").header("X-TSZ-Spelling-Speech","v1").header("X-TSZ-Phoneme-Speech","v1").body(Body::from(json!({"schema_version":3,"base_revision":read["word"]["revision"],"intent":"save","content":forms}).to_string())).unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let saved: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status, StatusCode::OK, "{saved}");
    let group = Uuid::now_v7();
    let input=serde_json::from_value(json!({"base_revision":0,"group":{"id":group,"name":"words","areas":["word","azure"],"uk":[{"id":Uuid::now_v7(),"name":"UK","voice_alias":"sonia","selected":true,"parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}}],"us":[{"id":Uuid::now_v7(),"name":"US","voice_alias":"jenny","selected":true,"parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}}]}})).unwrap();
    tsz_rust::speech::library::VoiceLibraryRepository::new(pool.clone())
        .save_group(actor, 0, input, true)
        .await
        .unwrap();
    sqlx::raw_sql("UPDATE speech.dubbing_update_settings SET enabled=true; UPDATE speech.dubbing_audio_updates SET due_at=now()-interval '1 second'").execute(&pool).await.unwrap();
    let provider = Arc::new(Provider(AtomicUsize::new(0)));
    let storage = MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    );
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    let fresh = latest(&state, &bearer, id).await;
    let variant = fresh["word"]["forms"].pointer(path).unwrap();
    for suffix in [
        "/spelling_speech",
        "/pronunciations/0/synthesis/uk/ipa_speech",
        "/pronunciations/0/synthesis/uk/ups_speech",
    ] {
        assert!(
            variant.pointer(suffix).unwrap()["voice_profile"]["voices"][0]["audio_asset_id"]
                .is_string(),
            "{suffix}: {fresh}"
        );
    }
    let stored: Value = sqlx::query_scalar(
        "SELECT content FROM lexicon.v3_form_contents WHERE entry_id=$1 AND id=$2",
    )
    .bind(entry)
    .bind(content_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stored.to_string().contains("audio_asset_id"));
    let refs:i64=sqlx::query_scalar("SELECT count(*) FROM lexicon.v3_audio_asset_references WHERE entry_id=$1 AND scope='draft'").bind(entry).fetch_one(&pool).await.unwrap();
    assert_eq!(refs, provider.0.load(Ordering::SeqCst) as i64);
    let jobs = audio_updates::list(&pool, group).await.unwrap();
    assert_eq!(jobs.updates[0].state, "completed", "{jobs:?}");
    assert_eq!(jobs.updates[0].total, 3);

    let recording = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id) VALUES($1,$2,$2,'audio/wav',48,'en-GB','female','recording.wav',$3)")
        .bind(recording).bind(format!("assets/{recording}.wav")).bind(actor).execute(&pool).await.unwrap();
    let mut forms = fresh["word"]["forms"].clone();
    let variant = forms.pointer_mut(path).unwrap();
    variant["spelling_speech"]["audio_assets"] = json!([{"id":recording,"locale":"en-US","gender":"male","content_type":"audio/mpeg","size_bytes":1,"duration_ms":null,"original_name":"forged.mp3","created_at":"2026-10-27T00:00:00Z"}]);
    let request=Request::builder().method(Method::PUT).uri(format!("{ROOT}/entries/{id}/steps/forms")).header(header::AUTHORIZATION,format!("Bearer {bearer}")).header(header::CONTENT_TYPE,"application/json").header("X-TSZ-Form-Contents","v1").header("X-TSZ-Voice-Versions","v3").header("X-TSZ-Voice-Locales","v1").header("X-TSZ-Voice-Models","v1").header("X-TSZ-Spelling-Speech","v1").header("X-TSZ-Phoneme-Speech","v1").body(Body::from(json!({"schema_version":3,"base_revision":fresh["word"]["revision"],"intent":"save","content":forms}).to_string())).unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let read = latest(&state, &bearer, id).await;
    let asset = &read["word"]["forms"].pointer(path).unwrap()["spelling_speech"]["audio_assets"][0];
    assert_eq!(asset["id"], recording.to_string());
    assert_eq!(asset["original_name"], "recording.wav");
    assert_eq!(asset["locale"], "en-GB");
    let referenced: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon.v3_audio_asset_references WHERE entry_id=$1 AND asset_id=$2 AND scope='draft')").bind(entry).bind(recording).fetch_one(&pool).await.unwrap();
    assert!(referenced);
}
