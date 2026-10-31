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
        "submitted updates must wait until due without any enable step"
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
    let no_generation = audio_updates::list(&pool, id)
        .await
        .unwrap()
        .updates
        .remove(0);
    assert_eq!(no_generation.completed, 0);
    assert_eq!(no_generation.skipped, 1);
    assert!(no_generation.error_codes.is_empty());
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

// Migration tests prepare the data that existed before pairing support. Calling the
// current repository here would require tables newer than the schema under test.
async fn seed_historical_group_and_job(pool: &PgPool, actor: Uuid, mut group: DubbingGroup) {
    group.revision = 1;
    let mut tx = pool.begin().await.unwrap();
    let updated = sqlx::query("UPDATE speech.dubbing_groups SET groups=groups || $1::jsonb,revision=revision+1,updated_by=$2,updated_at=now() WHERE singleton")
        .bind(json!([&group])).bind(actor).execute(&mut *tx).await.unwrap();
    assert_eq!(updated.rows_affected(), 1);
    let inserted = sqlx::query("INSERT INTO speech.dubbing_audio_updates(id,group_id,group_name,group_revision,areas,created_by_admin_id,security_version,due_at) SELECT $1,$2,$3,$4,$5,$6,0,now()+make_interval(secs=>wait_seconds) FROM speech.dubbing_update_settings WHERE singleton")
        .bind(Uuid::now_v7()).bind(group.id).bind(&group.name).bind(group.revision)
        .bind(&group.areas).bind(actor).execute(&mut *tx).await.unwrap();
    assert_eq!(inserted.rows_affected(), 1);
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn delayed_updates_migration_is_reversible_until_queue_data_is_written(pool: PgPool) {
    sqlx::migrate!("./migrations")
        .run_to(20261027100000, &pool)
        .await
        .unwrap();
    let down = include_str!("../../migrations/20261027090000_dubbing_audio_updates.down.sql");
    let up = include_str!("../../migrations/20261027090000_dubbing_audio_updates.up.sql");
    sqlx::raw_sql(down).execute(&pool).await.unwrap();
    sqlx::raw_sql(up).execute(&pool).await.unwrap();
    let (actor, id, version, _, _) = setup(&pool).await;
    seed_historical_group_and_job(&pool, actor, group(id, version, 0)).await;
    let error = sqlx::raw_sql(down).execute(&pool).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("dubbing audio update data exists")
    );
    assert_eq!(
        audio_updates::list(&pool, id).await.unwrap().updates.len(),
        1
    );
}

#[sqlx::test(migrations = false)]
async fn automatic_updates_migration_preserves_settings_and_pending_jobs(pool: PgPool) {
    let migrator = sqlx::migrate!("./migrations");
    migrator.run_to(20261027100000, &pool).await.unwrap();
    sqlx::query(
        "UPDATE speech.dubbing_update_settings SET enabled=false,wait_seconds=90,revision=4",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (actor, id, version, provider, storage) = setup(&pool).await;
    let sentence = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    seed_historical_group_and_job(&pool, actor, group(id, version, 0)).await;
    let job = audio_updates::list(&pool, id)
        .await
        .unwrap()
        .updates
        .remove(0);
    let target = Uuid::now_v7();
    let inserted = sqlx::query("INSERT INTO speech.dubbing_audio_update_targets(id,update_id,owner_kind,owner_id,target_id,target_path) SELECT $1,$2,owner_kind,owner_id,target_id,target_path FROM speech.dubbing_dependencies WHERE owner_kind='sentence' AND owner_id=$3")
        .bind(target).bind(job.id).bind(sentence).execute(&pool).await.unwrap();
    assert_eq!(inserted.rows_affected(), 1);
    migrator.run_to(20261027110000, &pool).await.unwrap();
    let settings: (i32, i64) =
        sqlx::query_as("SELECT wait_seconds,revision FROM speech.dubbing_update_settings")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(settings, (90, 4));
    let preserved = audio_updates::list(&pool, id)
        .await
        .unwrap()
        .updates
        .remove(0);
    assert_eq!(preserved.id, job.id);
    assert_eq!(preserved.due_at, job.due_at);
    assert_eq!(preserved.state, "pending");

    let snapshot_sql = r#"SELECT jsonb_build_object(
        'settings', (SELECT jsonb_agg(to_jsonb(s)) FROM speech.dubbing_update_settings s),
        'jobs', (SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM speech.dubbing_audio_updates j),
        'targets', (SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM speech.dubbing_audio_update_targets t),
        'ledger', (SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m),
        'columns', (SELECT jsonb_agg(to_jsonb(c) ORDER BY ordinal_position) FROM information_schema.columns c
            WHERE table_schema='speech' AND table_name='dubbing_update_settings'),
        'constraints', (SELECT jsonb_agg(jsonb_build_array(conname, pg_get_constraintdef(oid)) ORDER BY conname)
            FROM pg_constraint WHERE conrelid='speech.dubbing_update_settings'::regclass))"#;
    let before: Value = sqlx::query_scalar(snapshot_sql)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        before["targets"].as_array().map(Vec::len),
        Some(1),
        "rollback refusal must cover a persisted target"
    );
    let error = tsz_rust::deployment_migrations::undo(&pool, 20261027100000, 20261027110000)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("automatic dubbing updates require manual recovery"));
    let after: Value = sqlx::query_scalar(snapshot_sql)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "refusing rollback must preserve schema, settings, tasks and ledger"
    );

    // The historical assertions above stay on the old schema; current worker
    // code runs only after upgrading the preserved job to its current schema.
    migrator.run(&pool).await.unwrap();
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let completed = audio_updates::list(&pool, id)
        .await
        .unwrap()
        .updates
        .remove(0);
    assert_eq!(completed.state, "completed");
    assert_eq!(completed.total, 1);
    assert_eq!(completed.completed, 1);
    let target_state: String =
        sqlx::query_scalar("SELECT state FROM speech.dubbing_audio_update_targets WHERE id=$1")
            .bind(target)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(target_state, "completed");
    let stored = current(&pool, sentence).await;
    let asset = Uuid::parse_str(stored["voices"][0]["audio_asset_id"].as_str().unwrap()).unwrap();
    let references: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentence_audio_asset_references WHERE sentence_id=$1 AND asset_id=$2 AND scope='draft'")
        .bind(sentence).bind(asset).fetch_one(&pool).await.unwrap();
    assert_eq!(
        references, 1,
        "generated audio must be persisted on the inherited draft"
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
    assert!(settings.get("enabled").is_none());
    let input = json!({"wait_seconds":0,"base_revision":0});
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
    // Retry exercises a real default change, not revival of a cancelled update by an unchanged save.
    save(&pool, actor, group(id, version, -27), 1).await;
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
async fn delayed_updates_switch_both_accents_only_after_every_asset_is_ready(pool: PgPool) {
    let (actor, id, version, _, storage) = setup(&pool).await;
    let sentence = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    let text = json!({"mode":"unified","common":{"id":Uuid::now_v7(),"origin":"manual","value":{"version":2,"text":"Hello","annotations":[]},"voice_profile":{"voices":[],"dubbing":{"mode":"inherit","area":"example"}}}});
    sqlx::query(
        "UPDATE lexicon.shared_sentences SET content=jsonb_set(content,'{en_text}',$2) WHERE id=$1",
    )
    .bind(sentence)
    .bind(&text)
    .execute(&pool)
    .await
    .unwrap();
    tsz_rust::speech::dubbing::replace_dependencies(
        &mut pool.acquire().await.unwrap(),
        "sentence",
        sentence,
        1,
        &text,
    )
    .await
    .unwrap();
    save(&pool, actor, group(id, version, 0), 0).await;
    due(&pool).await;
    let provider = Arc::new(FailSecondProvider(AtomicUsize::new(0)));
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(provider.0.load(Ordering::SeqCst), 2);
    assert_eq!(
        sqlx::query_scalar::<_,Value>("SELECT content->'en_text'->'common'->'voice_profile'->'voices' FROM lexicon.shared_sentences WHERE id=$1").bind(sentence).fetch_one(&pool).await.unwrap(),
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
    let saved:Value=sqlx::query_scalar("SELECT content->'en_text'->'common'->'voice_profile' FROM lexicon.shared_sentences WHERE id=$1").bind(sentence).fetch_one(&pool).await.unwrap();
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
    assert_eq!(before, 2, "returning to -27 reuses its earlier audio bytes");
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
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        before,
        "returning to the global -10 effect reuses its earlier audio bytes"
    );
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

#[sqlx::test]
async fn overlapping_groups_and_order_restart_one_window_and_resolve_only_final_effect(
    pool: PgPool,
) {
    use tsz_rust::speech::library::dto::SaveDubbingGroupOrderRequest;
    let (actor, a, version, provider, storage) = setup(&pool).await;
    let owner = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    let independent = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"local","area":"example"}}),
    )
    .await;
    save(&pool, actor, group(a, version, 0), 0).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    let original = current(&pool, owner).await;
    let b = Uuid::now_v7();
    let b_version = Uuid::now_v7();
    let mut b_group = group(b, b_version, -27);
    b_group.name = "second group".into();
    save(&pool, actor, b_group.clone(), 0).await;
    // The old group job is already due, but a later overlapping commit postpones it.
    due(&pool).await;
    let mut renamed = group(a, version, 0);
    renamed.name = "first renamed".into();
    save(&pool, actor, renamed, 1).await;
    assert_eq!(
        audio_updates::list(&pool, b).await.unwrap().updates[0].state,
        "superseded"
    );
    assert_eq!(
        audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
            .await
            .unwrap(),
        0
    );
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "lower priority content and pure names do not call TTS"
    );
    assert_eq!(
        current(&pool, owner).await["voices"][0]["audio_asset_id"],
        original["voices"][0]["audio_asset_id"]
    );
    let repo = VoiceLibraryRepository::new(pool.clone());
    b_group.uk[0].parameters.rate_percent = 0;
    save(&pool, actor, b_group.clone(), 1).await;
    let before_switch = repo.groups_v3(actor).await.unwrap();
    let global = before_switch
        .groups
        .iter()
        .find(|g| g.is_global)
        .unwrap()
        .id;
    repo.save_order(
        actor,
        0,
        SaveDubbingGroupOrderRequest {
            group_ids: vec![b, a, global],
            base_revision: before_switch.order_revision,
        },
    )
    .await
    .unwrap();
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    let same_effect = current(&pool, owner).await;
    assert_eq!(same_effect["voices"][0]["version_id"], json!(b_version));
    assert_eq!(
        same_effect["voices"][0]["audio_asset_id"],
        original["voices"][0]["audio_asset_id"]
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "a different source identity with the same effect must reuse audio"
    );
    b_group.uk[0].parameters.rate_percent = -27;
    save(&pool, actor, b_group.clone(), 2).await;
    let read = repo.groups_v3(actor).await.unwrap();
    let global = read.groups.iter().find(|g| g.is_global).unwrap().id;
    let order = repo
        .save_order(
            actor,
            0,
            SaveDubbingGroupOrderRequest {
                group_ids: vec![b, a, global],
                base_revision: read.order_revision,
            },
        )
        .await
        .unwrap();
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    let switched = current(&pool, owner).await;
    assert_eq!(switched["voices"][0]["version_id"], json!(b_version));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let interim = repo
        .save_order(
            actor,
            0,
            SaveDubbingGroupOrderRequest {
                group_ids: vec![a, b, global],
                base_revision: order.order_revision,
            },
        )
        .await
        .unwrap();
    due(&pool).await;
    repo.save_order(
        actor,
        0,
        SaveDubbingGroupOrderRequest {
            group_ids: vec![b, a, global],
            base_revision: interim.order_revision,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
            .await
            .unwrap(),
        0
    );
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "returning to the current final effect makes no call"
    );
    assert_eq!(current(&pool, owner).await, switched);
    assert_eq!(current(&pool, independent).await["voices"], json!([]));
    // A bridge commit must merge the transitive overlap, including removed areas.
    let mut wide = group(a, version, 0);
    wide.areas = vec!["example".into(), "meaning".into()];
    save(&pool, actor, wide, 2).await;
    let c = Uuid::now_v7();
    let mut detached = group(c, Uuid::now_v7(), 0);
    detached.name = "detached".into();
    detached.areas = vec!["word".into()];
    save(&pool, actor, detached, 0).await;
    b_group.areas = vec!["meaning".into(), "word".into()];
    save(&pool, actor, b_group, 3).await;
    let active:Vec<(Uuid,Vec<String>)>=sqlx::query_as("SELECT group_id,areas FROM speech.dubbing_audio_updates WHERE state IN ('pending','running')").fetch_all(&pool).await.unwrap();
    assert_eq!(
        active,
        vec![(b, vec!["example".into(), "meaning".into(), "word".into()])]
    );
}

#[sqlx::test]
async fn order_supersedes_inflight_other_group_and_keeps_old_audio(pool: PgPool) {
    use tsz_rust::speech::library::dto::SaveDubbingGroupOrderRequest;
    let (actor, a, version, provider, storage) = setup(&pool).await;
    let owner = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    save(&pool, actor, group(a, version, 0), 0).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    let old = current(&pool, owner).await;
    let b = Uuid::now_v7();
    let mut other = group(b, Uuid::now_v7(), -10);
    other.name = "other".into();
    save(&pool, actor, other, 0).await;
    save(&pool, actor, group(a, version, -27), 1).await;
    due(&pool).await;
    let blocking = Arc::new(BlockingProvider {
        calls: AtomicUsize::new(0),
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let running = {
        let pool = pool.clone();
        let storage = storage.clone();
        let provider = blocking.clone();
        tokio::spawn(async move {
            audio_updates::sweep(&pool, Some(provider), Some(storage))
                .await
                .unwrap()
        })
    };
    tokio::time::timeout(Duration::from_secs(10), blocking.started.notified())
        .await
        .unwrap();
    let repo = VoiceLibraryRepository::new(pool.clone());
    let read = repo.groups_v3(actor).await.unwrap();
    let global = read.groups.iter().find(|g| g.is_global).unwrap().id;
    repo.save_order(
        actor,
        0,
        SaveDubbingGroupOrderRequest {
            group_ids: vec![b, a, global],
            base_revision: read.order_revision,
        },
    )
    .await
    .unwrap();
    blocking.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        current(&pool, owner).await,
        old,
        "superseded late response must preserve the full old profile"
    );
    // A failed replacement also preserves the old complete target.
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='speech.generate'").bind(actor).execute(&pool).await.unwrap();
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider), Some(storage))
        .await
        .unwrap();
    assert_eq!(current(&pool, owner).await, old);
}

#[sqlx::test]
async fn unchanged_defaults_never_generate_missing_audio_and_real_changes_survive_merging(
    pool: PgPool,
) {
    use tsz_rust::speech::library::dto::SaveDubbingGroupOrderRequest;
    let (actor, a, version, provider, storage) = setup(&pool).await;
    save(&pool, actor, group(a, version, 0), 0).await;
    // A pending update may be cancelled while its saved defaults remain in effect.
    sqlx::query("UPDATE speech.dubbing_audio_updates SET state='cancelled' WHERE state='pending'")
        .execute(&pool)
        .await
        .unwrap();
    let owner = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    let mut renamed = group(a, version, 0);
    renamed.name = "renamed A".into();
    save(&pool, actor, renamed.clone(), 1).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "names cannot fill a previously missing asset"
    );
    assert_eq!(current(&pool, owner).await["voices"], json!([]));
    let b = Uuid::now_v7();
    let mut lower = group(b, Uuid::now_v7(), -10);
    lower.name = "lower B".into();
    save(&pool, actor, lower.clone(), 0).await;
    let c = Uuid::now_v7();
    let mut lowest = group(c, Uuid::now_v7(), -20);
    lowest.name = "lower C".into();
    save(&pool, actor, lowest, 0).await;
    let repo = VoiceLibraryRepository::new(pool.clone());
    let read = repo.groups_v3(actor).await.unwrap();
    let global = read.groups.iter().find(|g| g.is_global).unwrap().id;
    repo.save_order(
        actor,
        0,
        SaveDubbingGroupOrderRequest {
            group_ids: vec![a, c, b, global],
            base_revision: read.order_revision,
        },
    )
    .await
    .unwrap();
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "lower priority additions and reorder keep the actual default"
    );
    renamed.uk[0].parameters.rate_percent = -27;
    save(&pool, actor, renamed, 2).await;
    lower.name = "B renamed after A change".into();
    save(&pool, actor, lower, 1).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "a later rename must preserve the earlier real change baseline"
    );
    assert_eq!(
        current(&pool, owner).await["voices"][0]["rate_percent"],
        -27
    );
}

#[sqlx::test]
async fn unchanged_defaults_do_not_fill_a_missing_side_of_a_partial_inherit_profile(pool: PgPool) {
    let (actor, a, version, provider, storage) = setup(&pool).await;
    let owner = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    let initial = group(a, version, 0);
    save(&pool, actor, initial.clone(), 0).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let mut variant: Value = sqlx::query_scalar(
        "SELECT content->'en_text'->'uk'->'variant' FROM lexicon.shared_sentences WHERE id=$1",
    )
    .bind(owner)
    .fetch_one(&pool)
    .await
    .unwrap();
    variant["voice_profile"]["voices"].as_array_mut().unwrap().push(json!({"voice_id":"jenny","locale":"en-US","version_id":initial.us[0].id,"version_name":"American","enabled":true,"rate_percent":0}));
    let unified = json!({"mode":"unified","common":variant});
    sqlx::query(
        "UPDATE lexicon.shared_sentences SET content=jsonb_set(content,'{en_text}',$2) WHERE id=$1",
    )
    .bind(owner)
    .bind(&unified)
    .execute(&pool)
    .await
    .unwrap();
    tsz_rust::speech::dubbing::replace_dependencies(
        &mut pool.acquire().await.unwrap(),
        "sentence",
        owner,
        2,
        &unified,
    )
    .await
    .unwrap();
    let mut renamed = initial;
    renamed.name = "partial target rename".into();
    save(&pool, actor, renamed, 1).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "the missing US asset must not cause a call when the entire default is unchanged"
    );
    let stored: Value =
        sqlx::query_scalar("SELECT content->'en_text' FROM lexicon.shared_sentences WHERE id=$1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, unified);
}

#[sqlx::test]
async fn merging_preserves_unknown_historical_baselines_and_task_scope(pool: PgPool) {
    let (actor, a, version, _, _) = setup(&pool).await;
    save(&pool, actor, group(a, version, 0), 0).await;
    sqlx::query("UPDATE speech.dubbing_audio_updates SET state='cancelled'")
        .execute(&pool)
        .await
        .unwrap();
    let mut changed = group(a, version, -27);
    save(&pool, actor, changed.clone(), 1).await;
    let unknown = Uuid::now_v7();
    sqlx::query("INSERT INTO speech.dubbing_audio_updates(id,group_id,group_name,group_revision,areas,created_by_admin_id,security_version,due_at,created_at) VALUES($1,$2,'historical',1,ARRAY['example'],$3,0,now()+interval '1 minute',now()+interval '1 day')").bind(unknown).bind(Uuid::now_v7()).bind(actor).execute(&pool).await.unwrap();
    let scoped = Uuid::now_v7();
    sqlx::query("INSERT INTO speech.dubbing_audio_updates(id,group_id,group_name,group_revision,areas,created_by_admin_id,security_version,due_at,test_run) VALUES($1,$2,'isolated',1,ARRAY['example'],$3,0,now()+interval '1 minute','other-test-batch')").bind(scoped).bind(Uuid::now_v7()).bind(actor).execute(&pool).await.unwrap();
    for base in [2, 3] {
        changed.name = format!("rename {base}");
        save(&pool, actor, changed.clone(), base).await;
        let baseline:Value=sqlx::query_scalar("SELECT baseline_groups FROM speech.dubbing_audio_updates WHERE group_id=$1 AND state='pending'").bind(a).fetch_one(&pool).await.unwrap();
        assert_eq!(
            baseline.get("example"),
            Some(&Value::Null),
            "unknown must dominate regardless of a misleading created_at or a later merge"
        );
    }
    let state: String =
        sqlx::query_scalar("SELECT state FROM speech.dubbing_audio_updates WHERE id=$1")
            .bind(scoped)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        state, "pending",
        "another test scope cannot be merged or superseded"
    );
}

#[sqlx::test]
async fn baseline_migration_preserves_legacy_jobs_and_rejects_lossy_rollback(pool: PgPool) {
    let down = include_str!("../../migrations/20261031100000_dubbing_update_baselines.down.sql");
    let up = include_str!("../../migrations/20261031100000_dubbing_update_baselines.up.sql");
    sqlx::raw_sql(down).execute(&pool).await.unwrap();
    let (actor, a, version, _, _) = setup(&pool).await;
    seed_historical_group_and_job(&pool, actor, group(a, version, 0)).await;
    let owner = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    let job = audio_updates::list(&pool, a)
        .await
        .unwrap()
        .updates
        .remove(0)
        .id;
    sqlx::query("INSERT INTO speech.dubbing_audio_update_targets(id,update_id,owner_kind,owner_id,target_id,target_path,state) SELECT $1,$2,owner_kind,owner_id,target_id,target_path,'skipped' FROM speech.dubbing_dependencies WHERE owner_id=$3").bind(Uuid::now_v7()).bind(job).bind(owner).execute(&pool).await.unwrap();
    let old_targets: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(t)) FROM speech.dubbing_audio_update_targets t",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let before: Value =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(j)) FROM speech.dubbing_audio_updates j")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::raw_sql(up).execute(&pool).await.unwrap();
    let after: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(j)-'baseline_groups') FROM speech.dubbing_audio_updates j",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(after, before);
    let baseline: Value =
        sqlx::query_scalar("SELECT baseline_groups FROM speech.dubbing_audio_updates")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        baseline,
        json!({}),
        "old tasks must not infer a pre-submit configuration"
    );
    let targets: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(t)-'applied') FROM speech.dubbing_audio_update_targets t",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(targets, old_targets);
    let unknown: Option<bool> =
        sqlx::query_scalar("SELECT applied FROM speech.dubbing_audio_update_targets")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        unknown, None,
        "historical skipped is not proof that no profile was written"
    );
    sqlx::query("UPDATE speech.dubbing_audio_update_targets SET applied=false")
        .execute(&pool)
        .await
        .unwrap();
    let error = sqlx::raw_sql(down).execute(&pool).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Cannot discard saved dubbing update baselines")
    );
    save(&pool, actor, group(a, version, -27), 1).await;
    let snapshot: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM speech.dubbing_audio_updates j",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let error = sqlx::raw_sql(down).execute(&pool).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Cannot discard saved dubbing update baselines")
    );
    let retained: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM speech.dubbing_audio_updates j",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(retained, snapshot);
}

#[sqlx::test]
async fn baseline_does_not_hide_partially_applied_effects_when_a_running_batch_reverts(
    pool: PgPool,
) {
    let (actor, a, version, provider, storage) = setup(&pool).await;
    let mut owners = vec![];
    for _ in 0..9 {
        owners.push(
            sentence(
                &pool,
                actor,
                json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
            )
            .await,
        );
    }
    save(&pool, actor, group(a, version, 0), 0).await;
    due(&pool).await;
    while audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap()
        > 0
    {}
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    save(&pool, actor, group(a, version, -27), 1).await;
    due(&pool).await;
    assert_eq!(
        audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
            .await
            .unwrap(),
        8
    );
    let partial = audio_updates::list(&pool, a)
        .await
        .unwrap()
        .updates
        .remove(0);
    assert_eq!(
        (partial.state.as_str(), partial.completed, partial.total),
        ("running", 8, 9)
    );
    save(&pool, actor, group(a, version, 0), 2).await;
    due(&pool).await;
    while audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap()
        > 0
    {}
    for owner in owners {
        assert_eq!(
            current(&pool, owner).await["voices"][0]["rate_percent"],
            0,
            "a target already changed by the old batch must be restored"
        );
    }
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "restoration reuses each target's earlier owned effect"
    );
}

#[sqlx::test]
async fn baseline_noop_jobs_keep_failed_updates_explicitly_retryable(pool: PgPool) {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt;
    let (actor, a, version, provider, storage) = setup(&pool).await;
    save(&pool, actor, group(a, version, 0), 0).await;
    sqlx::query("UPDATE speech.dubbing_audio_updates SET state='cancelled' WHERE state='pending'")
        .execute(&pool)
        .await
        .unwrap();
    let owner = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    save(&pool, actor, group(a, version, -27), 1).await;
    due(&pool).await;
    for _ in 0..3 {
        sqlx::query("UPDATE speech.dubbing_audio_update_targets SET retry_at=now()-interval '1 second' WHERE state='pending'").execute(&pool).await.unwrap();
        audio_updates::sweep(&pool, None, Some(storage.clone()))
            .await
            .unwrap();
    }
    let failed = audio_updates::list(&pool, a)
        .await
        .unwrap()
        .updates
        .remove(0);
    assert_eq!(failed.state, "failed");
    let state = tsz_rust::state::AppState::for_test(pool.clone());
    let retry = |id| {
        let token = state.admin_token_manager.generate(actor, "admin").unwrap();
        tsz_rust::router(state.clone()).oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/admin/speech/audio-updates/{id}/retry"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
    };
    let mut renamed = group(a, version, -27);
    renamed.name = "rename after failure".into();
    save(&pool, actor, renamed.clone(), 2).await;
    renamed.name = "another name only".into();
    save(&pool, actor, renamed, 3).await;
    assert_eq!(
        retry(failed.id).await.unwrap().status(),
        StatusCode::CONFLICT,
        "a newer active batch still takes priority"
    );
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "rename does not retry a paid operation implicitly"
    );
    assert_eq!(
        retry(failed.id).await.unwrap().status(),
        StatusCode::NO_CONTENT,
        "a no-effect completion cannot seal off a real failed update"
    );
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        current(&pool, owner).await["voices"][0]["rate_percent"],
        -27
    );
    // A later batch that actually changed audio must continue to block the older retry.
    save(&pool, actor, group(a, version, -20), 4).await;
    due(&pool).await;
    for _ in 0..3 {
        sqlx::query("UPDATE speech.dubbing_audio_update_targets SET retry_at=now()-interval '1 second' WHERE state='pending'").execute(&pool).await.unwrap();
        audio_updates::sweep(&pool, None, Some(storage.clone()))
            .await
            .unwrap();
    }
    let older = audio_updates::list(&pool, a)
        .await
        .unwrap()
        .updates
        .remove(0);
    assert_eq!(older.state, "failed");
    save(&pool, actor, group(a, version, -30), 5).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider), Some(storage))
        .await
        .unwrap();
    assert_eq!(
        retry(older.id).await.unwrap().status(),
        StatusCode::CONFLICT
    );
}

#[sqlx::test]
async fn cancelled_changes_are_not_revived_by_names_when_old_audio_still_exists(pool: PgPool) {
    let (actor, a, version, provider, storage) = setup(&pool).await;
    let owner = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    save(&pool, actor, group(a, version, 0), 0).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap();
    let original = current(&pool, owner).await;
    save(&pool, actor, group(a, version, -27), 1).await;
    sqlx::query("UPDATE speech.dubbing_audio_updates SET state='cancelled' WHERE state='pending'")
        .execute(&pool)
        .await
        .unwrap();
    let mut renamed = group(a, version, -27);
    renamed.name = "name after cancellation".into();
    save(&pool, actor, renamed, 2).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider.clone()), Some(storage))
        .await
        .unwrap();
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "a rename cannot revive a cancelled real change"
    );
    assert_eq!(current(&pool, owner).await, original);
}

async fn legacy_profile_with_two_assets(
    actor: Uuid,
    service: &GeneratedAudioService,
    a: Uuid,
    b: Uuid,
) -> Value {
    let mut voices = vec![];
    for (version, rate) in [(a, 0), (b, -27)] {
        let mut request = input(Uuid::now_v7(), version, "Hello");
        request.setting.rate_percent = rate;
        let mut setting = request.setting.clone();
        setting.audio_asset_id = Some(service.generate(actor, 0, request).await.unwrap().asset.id);
        voices.push(setting);
    }
    json!({"voices":voices,"dubbing":{"mode":"inherit","area":"example"}})
}

async fn link_legacy_profile_assets(pool: &PgPool, owner: Uuid, profile: &Value) {
    for setting in profile["voices"].as_array().unwrap() {
        let asset = Uuid::parse_str(setting["audio_asset_id"].as_str().unwrap()).unwrap();
        sqlx::query("INSERT INTO lexicon.shared_sentence_audio_asset_references(asset_id,sentence_id,scope,variant_id) SELECT $1,$2,'draft',target_id FROM speech.dubbing_dependencies WHERE owner_kind='sentence' AND owner_id=$2 ON CONFLICT DO NOTHING").bind(asset).bind(owner).execute(pool).await.unwrap();
    }
}

#[sqlx::test]
async fn reused_assets_are_application_facts_when_a_partially_applied_batch_reverts(pool: PgPool) {
    let (actor, a, version, provider, storage) = setup(&pool).await;
    let second = Uuid::now_v7();
    let mut values = group(a, version, 0);
    let mut extra = values.uk[0].clone();
    extra.id = second;
    extra.name = "second".into();
    extra.parameters.rate_percent = -27;
    values.uk.push(extra);
    save(&pool, actor, values.clone(), 0).await;
    sqlx::query("UPDATE speech.dubbing_audio_updates SET state='cancelled' WHERE state='pending'")
        .execute(&pool)
        .await
        .unwrap();
    let service =
        GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(storage.clone()));
    let mut owners = vec![];
    for _ in 0..9 {
        let profile = legacy_profile_with_two_assets(actor, &service, version, second).await;
        let owner = sentence(&pool, actor, profile.clone()).await;
        link_legacy_profile_assets(&pool, owner, &profile).await;
        owners.push(owner);
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    values.uk.swap(0, 1);
    save(&pool, actor, values.clone(), 1).await;
    due(&pool).await;
    assert_eq!(
        audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
            .await
            .unwrap(),
        8
    );
    let partial = audio_updates::list(&pool, a)
        .await
        .unwrap()
        .updates
        .remove(0);
    assert_eq!(
        (
            partial.state.as_str(),
            partial.skipped,
            partial.completed,
            partial.total
        ),
        ("running", 8, 0, 9),
        "generation counters keep their old semantics even though eight profiles changed"
    );
    values.uk.swap(0, 1);
    save(&pool, actor, values, 2).await;
    due(&pool).await;
    while audio_updates::sweep(&pool, Some(provider.clone()), Some(storage.clone()))
        .await
        .unwrap()
        > 0
    {}
    for owner in owners {
        assert_eq!(
            current(&pool, owner).await["voices"][0]["rate_percent"],
            0,
            "asset reuse must not hide a committed switch to the other default"
        );
    }
    let writes: i64 = sqlx::query_scalar("SELECT count(*) FROM speech.dubbing_audio_update_targets WHERE update_id=$1 AND applied IS TRUE").bind(partial.id).fetch_one(&pool).await.unwrap();
    assert_eq!(
        writes, 8,
        "reusing assets still records each actual profile write"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}

#[sqlx::test]
async fn retry_cannot_cross_a_successor_that_applied_reused_audio(pool: PgPool) {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt;
    let (actor, a, version, provider, storage) = setup(&pool).await;
    let second = Uuid::now_v7();
    let mut values = group(a, version, 0);
    let mut extra = values.uk[0].clone();
    extra.id = second;
    extra.name = "second".into();
    extra.parameters.rate_percent = -27;
    values.uk.push(extra);
    save(&pool, actor, values.clone(), 0).await;
    sqlx::query("UPDATE speech.dubbing_audio_updates SET state='cancelled' WHERE state='pending'")
        .execute(&pool)
        .await
        .unwrap();
    let owner = sentence(
        &pool,
        actor,
        json!({"voices":[],"dubbing":{"mode":"inherit","area":"example"}}),
    )
    .await;
    values.uk.swap(0, 1);
    save(&pool, actor, values.clone(), 1).await;
    due(&pool).await;
    for _ in 0..3 {
        sqlx::query("UPDATE speech.dubbing_audio_update_targets SET retry_at=now()-interval '1 second' WHERE state='pending'").execute(&pool).await.unwrap();
        audio_updates::sweep(&pool, None, Some(storage.clone()))
            .await
            .unwrap();
    }
    let failed = audio_updates::list(&pool, a)
        .await
        .unwrap()
        .updates
        .remove(0);
    assert_eq!(failed.state, "failed");
    let service =
        GeneratedAudioService::new(pool.clone(), Some(provider.clone()), Some(storage.clone()));
    let profile = legacy_profile_with_two_assets(actor, &service, version, second).await;
    sqlx::query("UPDATE lexicon.shared_sentences SET content=jsonb_set(content,'{en_text,uk,variant,voice_profile}',$2) WHERE id=$1").bind(owner).bind(&profile).execute(&pool).await.unwrap();
    link_legacy_profile_assets(&pool, owner, &profile).await;
    values.name = "reuse saved versions".into();
    save(&pool, actor, values, 2).await;
    due(&pool).await;
    audio_updates::sweep(&pool, Some(provider), Some(storage))
        .await
        .unwrap();
    let later = audio_updates::list(&pool, a)
        .await
        .unwrap()
        .updates
        .remove(0);
    assert_eq!(
        (later.state.as_str(), later.skipped, later.completed),
        ("completed", 1, 0)
    );
    assert_eq!(
        current(&pool, owner).await["voices"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        current(&pool, owner).await["voices"][0]["version_id"],
        json!(second)
    );
    let state = tsz_rust::state::AppState::for_test(pool.clone());
    let token = state.admin_token_manager.generate(actor, "admin").unwrap();
    let response = tsz_rust::router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/speech/audio-updates/{}/retry",
                    failed.id
                ))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "a reused asset was actually applied, so the older failure is obsolete"
    );
}
