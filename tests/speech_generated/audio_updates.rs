use super::*;
use serde_json::Value;
use tsz_rust::speech::{
    audio_updates,
    library::{
        VoiceLibraryRepository,
        dto::{DubbingGroup, SaveDubbingGroupRequest},
    },
};

async fn setup(pool: &PgPool) -> (Uuid, Uuid, Uuid, Arc<Provider>, Arc<dyn ObjectStore>) {
    let actor = actor(pool).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) SELECT $1,unnest(ARRAY['speech.global_dubbing','sentences.access','sentences.edit']),$1").bind(actor).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO speech.voices(id,alias,provider,provider_voice_id,locale,gender,styles,provider_version) VALUES($1,'jenny','azure','en-US-JennyNeural','en-US','female','[]','v1')").bind(Uuid::now_v7()).execute(pool).await.unwrap();
    let group = Uuid::now_v7();
    let version = Uuid::now_v7();
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: false,
    });
    let storage = store();
    (actor, group, version, provider, storage)
}
fn group(id: Uuid, version: Uuid, rate: i16) -> DubbingGroup {
    serde_json::from_value(json!({"id":id,"name":"examples","areas":["example"],"revision":0,"is_global":false,
        "uk":[{"id":version,"name":"British","voice_alias":"sonia","selected":true,"parameters":{"rate_percent":rate,"pitch_semitones":0,"style":null}}],
        "us":[{"id":Uuid::from_u128(version.as_u128()+1),"name":"American","voice_alias":"jenny","selected":true,"parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}}]})).unwrap()
}
async fn save(pool: &PgPool, actor: Uuid, group: DubbingGroup, revision: i64) {
    let create = revision == 0 && !group.is_global;
    VoiceLibraryRepository::new(pool.clone())
        .save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group,
                base_revision: revision,
            },
            create,
        )
        .await
        .unwrap();
}
async fn sentence(pool: &PgPool, actor: Uuid, profile: Value) -> Uuid {
    let id = Uuid::now_v7();
    let target = Uuid::now_v7();
    let text = json!({"mode":"distinguish","source_dialect":"uk","uk":{"state":"ready","variant":{"id":target,"origin":"manual","value":{"version":2,"text":"Hello","annotations":[]},"voice_profile":profile}},"us":{"state":"missing"}});
    sqlx::query("INSERT INTO lexicon.shared_sentences(id,content,create_digest,created_by_admin_id) VALUES($1,$2,'fixture',$3)").bind(id).bind(json!({"id":id,"level":"A1","en_text":text,"links":[]})).bind(actor).execute(pool).await.unwrap();
    tsz_rust::speech::dubbing::replace_dependencies(
        &mut pool.acquire().await.unwrap(),
        "sentence",
        id,
        1,
        &text,
    )
    .await
    .unwrap();
    id
}
async fn current(pool: &PgPool, id: Uuid) -> Value {
    sqlx::query_scalar("SELECT content->'en_text'->'uk'->'variant'->'voice_profile' FROM lexicon.shared_sentences WHERE id=$1").bind(id).fetch_one(pool).await.unwrap()
}
async fn due(pool: &PgPool) {
    sqlx::query("UPDATE speech.dubbing_update_settings SET enabled=true")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE speech.dubbing_audio_updates SET due_at=now()-interval '1 second' WHERE state='pending'").execute(pool).await.unwrap();
}

#[sqlx::test]
async fn test_run_jobs_keep_scope_and_never_fall_back_to_normal_storage(pool: PgPool) {
    let (actor, id, version, provider, storage) = setup(&pool).await;
    let owned = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    let other = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id: other,
            phone: format!("scope-{other}"),
            display_name: "other".into(),
            password_hash: "hash".into(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    let foreign = sentence(
        &pool,
        other,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    save(&pool, actor, group(id, version, 0), 0).await;
    sqlx::query("UPDATE speech.dubbing_audio_updates SET test_run='isolated-fixture'")
        .execute(&pool)
        .await
        .unwrap();
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(current(&pool, owned).await["voices"], json!([]));
    assert_eq!(current(&pool, foreign).await["voices"], json!([]));
    let job = audio_updates::list(&pool, id)
        .await
        .unwrap()
        .updates
        .remove(0);
    assert_eq!(
        job.total, 1,
        "test jobs cannot include other accounts' data"
    );
    assert!(
        job.error_codes
            .contains(&"speech_storage_unavailable".to_owned())
    );
}

struct BlockingProvider {
    calls: AtomicUsize,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl SpeechProvider for BlockingProvider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }
    async fn synthesize(&self, _: &SynthesisRequest) -> Result<SynthesizedAudio, SpeechError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        self.release.notified().await;
        Ok(SynthesizedAudio {
            bytes: b"ID3-delayed-test".to_vec(),
            content_type: "audio/mpeg",
            provider_request_id: None,
        })
    }
}

#[sqlx::test]
async fn delayed_updates_reject_late_results_after_content_or_recommendations_change(pool: PgPool) {
    let (actor, id, version, _, storage) = setup(&pool).await;
    let sentence = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    save(&pool, actor, group(id, version, 0), 0).await;
    due(&pool).await;
    for edit_content in [false, true] {
        let provider = Arc::new(BlockingProvider {
            calls: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let running = {
            let pool = pool.clone();
            let provider = provider.clone();
            let storage = storage.clone();
            tokio::spawn(async move {
                audio_updates::sweep(&pool, Some(provider), Some(storage))
                    .await
                    .unwrap()
            })
        };
        tokio::time::timeout(Duration::from_secs(10), provider.started.notified())
            .await
            .unwrap();
        if edit_content {
            sqlx::query("UPDATE lexicon.shared_sentences SET revision=revision+1,content=jsonb_set(content,'{en_text,uk,variant,value,text}','\"Changed\"') WHERE id=$1").bind(sentence).execute(&pool).await.unwrap();
        } else {
            save(&pool, actor, group(id, version, -27), 1).await;
        }
        provider.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            current(&pool, sentence).await["voices"],
            json!([]),
            "late audio must never bind"
        );
        if !edit_content {
            due(&pool).await;
        }
    }
    assert_eq!(
        audio_updates::list(&pool, id).await.unwrap().updates[0].skipped,
        1
    );
}

#[sqlx::test]
async fn delayed_updates_recover_expired_leases_and_do_not_repeat_completed_generation(
    pool: PgPool,
) {
    let (actor, id, version, provider, storage) = setup(&pool).await;
    let sentence = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    save(&pool, actor, group(id, version, 0), 0).await;
    due(&pool).await;
    let job = audio_updates::list(&pool, id).await.unwrap().updates[0].id;
    sqlx::query("INSERT INTO speech.dubbing_audio_update_targets(id,update_id,owner_kind,owner_id,target_id,target_path,state,attempts,lease_token,lease_until) SELECT $1,$2,owner_kind,owner_id,target_id,target_path,'running',1,$3,now()-interval '1 second' FROM speech.dubbing_dependencies WHERE owner_id=$4").bind(Uuid::now_v7()).bind(job).bind(Uuid::now_v7()).bind(sentence).execute(&pool).await.unwrap();
    sqlx::query("UPDATE speech.dubbing_audio_updates SET state='running' WHERE id=$1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert!(current(&pool, sentence).await["voices"][0]["audio_asset_id"].is_string());
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        audio_updates::list(&pool, id).await.unwrap().updates[0].state,
        "completed"
    );
}

#[sqlx::test]
async fn delayed_updates_merge_and_reuse_final_effects_without_touching_publications(pool: PgPool) {
    let (actor, id, version, provider, storage) = setup(&pool).await;
    let profile = json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}});
    let sentence = sentence(&pool, actor, profile).await;
    save(&pool, actor, group(id, version, 0), 0).await;
    assert_eq!(
        audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
            .await
            .unwrap(),
        0,
        "disabled queue must remain durable"
    );
    due(&pool).await;
    assert_eq!(
        audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
            .await
            .unwrap(),
        1
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let original = current(&pool, sentence).await;
    let asset = original["voices"][0]["audio_asset_id"].clone();
    assert!(
        asset.is_string(),
        "first update must bind an asset: {original}"
    );
    let public: Value =
        sqlx::query_scalar("SELECT content FROM lexicon.shared_sentences WHERE id=$1")
            .bind(sentence)
            .fetch_one(&pool)
            .await
            .unwrap();
    let publication = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.shared_sentence_publications(id,sentence_id,publication_number,source_revision,snapshot,published_by_admin_id) SELECT $1,id,1,revision,$3,$4 FROM lexicon.shared_sentences WHERE id=$2").bind(publication).bind(sentence).bind(&public).bind(actor).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.shared_sentences SET current_publication_id=$2 WHERE id=$1")
        .bind(sentence)
        .bind(publication)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO lexicon.shared_sentence_audio_asset_references(asset_id,sentence_id,scope,publication_id,variant_id) SELECT asset_id,sentence_id,'publication',$2,variant_id FROM lexicon.shared_sentence_audio_asset_references WHERE sentence_id=$1 AND scope='draft'").bind(sentence).bind(publication).execute(&pool).await.unwrap();
    save(&pool, actor, group(id, version, -27), 1).await;
    let mut renamed = group(id, version, 0);
    renamed.name = "new group name".into();
    renamed.uk[0].name = "new version name".into();
    save(&pool, actor, renamed, 2).await;
    let jobs = audio_updates::list(&pool, id).await.unwrap();
    assert_eq!(jobs.updates[0].state, "pending");
    assert_eq!(jobs.updates[1].state, "superseded");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "rename and reverting the effective rate must make zero calls"
    );
    let updated = current(&pool, sentence).await;
    assert_eq!(updated["voices"][0]["audio_asset_id"], asset);
    assert_eq!(updated["voices"][0]["version_name"], "new version name");
    let frozen: Value =
        sqlx::query_scalar("SELECT snapshot FROM lexicon.shared_sentence_publications WHERE id=$1")
            .bind(publication)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(public, frozen);
    assert_eq!(
        audio_updates::list(&pool, id).await.unwrap().updates[0].state,
        "completed"
    );
    let references:i64=sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentence_audio_asset_references WHERE sentence_id=$1 AND scope='draft'").bind(sentence).fetch_one(&pool).await.unwrap();
    assert_eq!(references, 1);
    save(&pool, actor, group(id, version, -27), 3).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_ne!(
        current(&pool, sentence).await["voices"][0]["audio_asset_id"],
        asset
    );
}

#[sqlx::test]
async fn delayed_updates_recheck_permissions_and_exclude_independent_targets(pool: PgPool) {
    let (actor, id, version, provider, storage) = setup(&pool).await;
    let inherited = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    let local = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"local","area":"example"}}),
    )
    .await;
    save(&pool, actor, group(id, version, 0), 0).await;
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='speech.generate'").bind(actor).execute(&pool).await.unwrap();
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(current(&pool, inherited).await["voices"], json!([]));
    assert_eq!(current(&pool, local).await["dubbing"]["mode"], "local");
    let job = audio_updates::list(&pool, id)
        .await
        .unwrap()
        .updates
        .remove(0);
    assert_eq!(job.state, "failed");
    assert_eq!(job.total, 1);
    assert_eq!(job.error_codes, vec!["forbidden"]);
}

#[sqlx::test]
async fn delayed_updates_migration_is_reversible_until_queue_data_is_written(pool: PgPool) {
    let down = include_str!("../../migrations/20261027090000_dubbing_audio_updates.down.sql");
    let up = include_str!("../../migrations/20261027090000_dubbing_audio_updates.up.sql");
    sqlx::raw_sql(down).execute(&pool).await.unwrap();
    sqlx::raw_sql(up).execute(&pool).await.unwrap();
    let (actor, id, version, _, _) = setup(&pool).await;
    save(&pool, actor, group(id, version, 0), 0).await;
    assert!(sqlx::raw_sql(down).execute(&pool).await.is_err());
    assert_eq!(
        audio_updates::list(&pool, id).await.unwrap().updates.len(),
        1
    );
}

#[sqlx::test]
async fn delayed_updates_api_enforces_versions_cancel_boundary_and_retry(pool: PgPool) {
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode, header},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let (actor, id, version, provider, storage) = setup(&pool).await;
    let state = tsz_rust::state::AppState::for_test(pool.clone());
    async fn call(
        state: &tsz_rust::state::AppState,
        actor: Uuid,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let token = state.admin_token_manager.generate(actor, "admin").unwrap();
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
            .unwrap();
        let response = tsz_rust::router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap()
            },
        )
    }
    let root = "/api/v1/admin/speech";
    let path = format!("{root}/audio-update-settings");
    let (status, settings) = call(&state, actor, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    assert_eq!(settings["wait_seconds"], 60);
    let input = json!({"wait_seconds":0,"enabled":true,"base_revision":0});
    assert_eq!(
        call(&state, actor, Method::PUT, &path, Some(input.clone()))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&state, actor, Method::PUT, &path, Some(input)).await.0,
        StatusCode::CONFLICT
    );
    save(&pool, actor, group(id, version, 0), 0).await;
    let job = audio_updates::list(&pool, id).await.unwrap().updates[0].id;
    let cancel = format!("{root}/audio-updates/{job}/cancel");
    assert_eq!(
        call(&state, actor, Method::POST, &cancel, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&state, actor, Method::POST, &cancel, None).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    save(&pool, actor, group(id, version, 0), 1).await;
    let job = audio_updates::list(&pool, id).await.unwrap().updates[0].id;
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='speech.generate'").bind(actor).execute(&pool).await.unwrap();
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    let retry = format!("{root}/audio-updates/{job}/retry");
    assert_eq!(
        call(
            &state,
            actor,
            Method::POST,
            &format!("{root}/audio-updates/{job}/cancel"),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.generate',$1)").bind(actor).execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, actor, Method::POST, &retry, None).await.0,
        StatusCode::NO_CONTENT
    );
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='speech.global_dubbing'").bind(actor).execute(&pool).await.unwrap();
    assert_eq!(
        call(
            &state,
            actor,
            Method::GET,
            &format!("{root}/groups/{id}/audio-updates"),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

struct FailSecondProvider(AtomicUsize);
#[async_trait]
impl SpeechProvider for FailSecondProvider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }
    async fn synthesize(&self, _: &SynthesisRequest) -> Result<SynthesizedAudio, SpeechError> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 1 {
            return Err(SpeechError::new(SpeechErrorKind::Unavailable, None));
        }
        Ok(SynthesizedAudio {
            bytes: b"ID3-test".to_vec(),
            content_type: "audio/mpeg",
            provider_request_id: None,
        })
    }
}

#[sqlx::test]
async fn delayed_updates_switch_multiple_versions_only_after_every_asset_is_ready(pool: PgPool) {
    let (actor, id, version, _, storage) = setup(&pool).await;
    let sentence = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    let mut values = group(id, version, 0);
    let mut second = values.uk[0].clone();
    second.id = Uuid::now_v7();
    second.name = "Slower".into();
    second.parameters.rate_percent = -27;
    values.uk.push(second);
    save(&pool, actor, values, 0).await;
    due(&pool).await;
    let provider = Arc::new(FailSecondProvider(AtomicUsize::new(0)));
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(provider.0.load(Ordering::SeqCst), 2);
    assert_eq!(
        current(&pool, sentence).await["voices"],
        json!([]),
        "a ready first version must not replace only half the target"
    );
    sqlx::query("UPDATE speech.dubbing_audio_update_targets SET retry_at=now()-interval '1 second' WHERE state='pending'").execute(&pool).await.unwrap();
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(
        provider.0.load(Ordering::SeqCst),
        3,
        "retry reuses the already-ready first version"
    );
    let saved = current(&pool, sentence).await;
    assert_eq!(saved["voices"].as_array().unwrap().len(), 2);
    assert!(
        saved["voices"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["audio_asset_id"].is_string())
    );
}

#[sqlx::test]
async fn delayed_updates_global_fallback_move_and_delete_reparse_current_source(pool: PgPool) {
    use tsz_rust::speech::library::dto::DeleteDubbingGroupRequest;
    let (actor, id, version, provider, storage) = setup(&pool).await;
    let sentence = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    let repository = VoiceLibraryRepository::new(pool.clone());
    let global_id = repository.groups(actor).await.unwrap().groups[0].id;
    let mut global = group(global_id, Uuid::now_v7(), -10);
    global.name = "全局常用".into();
    global.areas.clear();
    global.is_global = true;
    save(&pool, actor, global, 0).await;
    save(&pool, actor, group(id, version, -27), 0).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(
        current(&pool, sentence).await["voices"][0]["rate_percent"],
        -27
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let mut moved = group(id, version, -27);
    moved.areas = vec!["word".into()];
    save(&pool, actor, moved, 1).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(
        current(&pool, sentence).await["voices"][0]["rate_percent"],
        -10
    );
    let unselected = {
        let mut values = group(id, version, -27);
        let mut extra = values.uk[0].clone();
        extra.id = Uuid::now_v7();
        extra.selected = false;
        extra.parameters.rate_percent = 25;
        values.uk.push(extra);
        values
    };
    save(&pool, actor, unselected, 2).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    let before = provider.calls.load(Ordering::SeqCst);
    assert_eq!(
        current(&pool, sentence).await["voices"][0]["rate_percent"],
        -27
    );
    repository
        .delete_group(actor, 0, id, DeleteDubbingGroupRequest { base_revision: 3 })
        .await
        .unwrap();
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(
        current(&pool, sentence).await["voices"][0]["rate_percent"],
        -10
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), before + 1);
}

#[sqlx::test]
async fn delayed_updates_provider_failure_retains_old_audio_until_retry(pool: PgPool) {
    let (actor, id, version, provider, storage) = setup(&pool).await;
    let sentence = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    save(&pool, actor, group(id, version, 0), 0).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider), Some(storage.clone()))
        .await
        .unwrap();
    let original = current(&pool, sentence).await;
    save(&pool, actor, group(id, version, -27), 1).await;
    let failing = Arc::new(Provider {
        calls: AtomicUsize::new(0),
        fail_first: true,
    });
    due(&pool).await;
    audio_updates::sweep(&pool, Some(failing.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(
        current(&pool, sentence).await,
        original,
        "failed generation cannot replace any version"
    );
    sqlx::query("UPDATE speech.dubbing_audio_update_targets SET retry_at=now()-interval '1 second' WHERE state='pending'").execute(&pool).await.unwrap();
    audio_updates::sweep(&pool, Some(failing.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(failing.calls.load(Ordering::SeqCst), 2);
    assert_ne!(current(&pool, sentence).await, original);
    assert_eq!(
        audio_updates::list(&pool, id).await.unwrap().updates[0].state,
        "completed"
    );
}
