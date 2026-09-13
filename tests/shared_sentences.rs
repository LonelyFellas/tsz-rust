use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    state::AppState,
};
use uuid::Uuid;
const ROOT: &str = "/api/v1/admin/lexicon/sentences";

async fn admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("shared-{id}"),
            display_name: "例句测试".into(),
            password_hash: "hashed".into(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    id
}
fn node_id(entry: Uuid, tag: u8) -> Uuid {
    let mut bytes = *entry.as_bytes();
    bytes[0] ^= tag;
    Uuid::from_bytes(bytes)
}
fn sense_id(entry: Uuid) -> Uuid {
    node_id(entry, 4)
}
fn target_ref(entry: Uuid) -> Value {
    json!({"state":"linked","target_entry_id":entry,"target_pos_id":node_id(entry,1),"target_base_form_id":node_id(entry,2),"target_form_id":node_id(entry,2),"target_variant_id":node_id(entry,3),"target_sense_id":sense_id(entry)})
}
async fn entry(pool: &PgPool, owner: Uuid, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,revision,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word',1,'{}',$2,$2)").bind(id).bind(owner).execute(pool).await.unwrap();
    let forms = json!({"pos":[{"pos_id":node_id(id,1),"pos":"verb","dialect_rules":{"spelling_mode":"unified","phonetic_mode":"unified"},"forms":[{"id":node_id(id,2),"form_type":"base","regional_variants":{"mode":"common","common":{"id":node_id(id,3),"dialect":"common","spelling":name,"origin":"manual","pronunciations":[]}}}],"form_groups":[{"id":node_id(id,5),"is_regular":true,"members":[{"id":node_id(id,9),"form_id":node_id(id,2)}]}]}]});
    let meanings = json!({"sense_groups":[],"pos":[{"pos_id":node_id(id,1),"grammar_structures":[],"senses":[{"id":sense_id(id),"sub_pos":"","level":"A1","depends_on_context":false,"definitions":[],"sentences":[],"relations":[]}]}]});
    sqlx::query("INSERT INTO lexicon.entry_editor_projection(entry_id,forms,meanings,rebuilt_revision) VALUES($1,$2,$3,1)").bind(id).bind(forms).bind(meanings).execute(pool).await.unwrap();
    for (tag, kind) in [
        (1, "pos"),
        (2, "concrete_form"),
        (3, "form_variant"),
        (4, "sense"),
    ] {
        sqlx::query("INSERT INTO lexicon.nodes(id,entry_id,node_type) VALUES($1,$2,$3)")
            .bind(node_id(id, tag))
            .bind(id)
            .bind(kind)
            .execute(pool)
            .await
            .unwrap();
    }
    for side in ["uk", "us"] {
        sqlx::query("INSERT INTO lexicon.surface_sources(entry_id,source_id,source_kind,source_node_id,language,entry_kind,dialect,dialect_scope,surface,normalized_surface,normalization_version,source_revision,content_scope,pos_id,pos,form_type,variant_id,form_id,group_ids,projection_version) VALUES($1,$2,'form_variant',$3,'en','word','common',$4,$5,$5,1,1,'draft',$6,'verb','base',$3,$7,ARRAY[$8]::uuid[],'test')")
            .bind(id).bind(format!("{id}-{side}")).bind(node_id(id,3)).bind(side).bind(name).bind(node_id(id,1)).bind(node_id(id,2)).bind(node_id(id,5)).execute(pool).await.unwrap();
    }
    sqlx::query("INSERT INTO lexicon.v3_entry_state(entry_id,origin,initial_headwords,initial_headword_keys) VALUES($1,'native',$2,$3)").bind(id).bind(json!({"mode":"unified","common":name})).bind(vec![format!("uk:{name}"),format!("us:{name}")]).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO lexicon.entry_presentation_projection(entry_id,content_schema_version,source_revision,label,matched_surfaces,strategy_version) VALUES($1,3,1,$2,ARRAY[$2]::text[],'test')").bind(id).bind(name).execute(pool).await.unwrap();
    id
}
async fn call(
    state: &AppState,
    actor: Uuid,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let token = state
        .admin_token_manager
        .generate(actor, AdminRole::Admin.as_str())
        .unwrap();
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("Idempotency-Key", Uuid::now_v7().to_string());
    let body = if let Some(body) = body {
        req = req.header(header::CONTENT_TYPE, "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let res = tsz_rust::router(state.clone())
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}
fn content(target: Uuid) -> Value {
    let translation = Uuid::now_v7();
    json!({"sentence":{"id":Uuid::now_v7(),"level":"B1","en_text":{"mode":"unified","common":{"id":Uuid::now_v7(),"origin":"manual","value":{"version":2,"text":"A wonderful flower.","annotations":[]}}},"zh_text_id":translation,"zh_text":{"version":2,"text":"一朵美丽的花。","annotations":[]},"zh_translations":[{"id":translation,"band":"balanced_fluency","language":"zh","content":{"version":2,"text":"一朵美丽的花。","annotations":[]}}],"links":[]},"annotations":[{"id":Uuid::now_v7(),"source_dialect":"common","source_segments":[{"start":2,"end":11,"surface":"wonderful"}],"target":target_ref(target)},{"id":Uuid::now_v7(),"source_dialect":"common","source_segments":[{"start":12,"end":18,"surface":"flower"}],"target":{"state":"pending","kind":"word","headword":"flower","gloss":"花"}}]})
}

#[sqlx::test]
async fn annotations_define_membership_and_unlink_preserves_shared_content(pool: PgPool) {
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let flower = entry(&pool, actor, "flower").await;
    let homograph = entry(&pool, actor, "flower").await;
    let state = AppState::for_test(pool.clone());
    let mut proposed = content(source);
    proposed["annotations"][1]["target"] = json!(target_ref(flower));
    let input =
        json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":proposed});
    let (status, saved) = call(&state, actor, Method::POST, ROOT, Some(input.clone())).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let id = saved["id"].as_str().unwrap();
    assert_eq!(saved["entries"].as_array().unwrap().len(), 2);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentence_collections")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "creation must not create a second membership record"
    );
    for target in [source, flower] {
        let (_, view) = call(
            &state,
            actor,
            Method::GET,
            &format!("{ROOT}?entry_id={target}"),
            None,
        )
        .await;
        assert_eq!(view["total"], 1);
        assert_eq!(view["items"][0]["id"], saved["id"]);
    }
    // A legacy collection without a Linked annotation cannot make a sentence appear.
    sqlx::query(
        "INSERT INTO lexicon.shared_sentence_collections(sentence_id,entry_id) VALUES($1,$2)",
    )
    .bind(Uuid::parse_str(id).unwrap())
    .bind(homograph)
    .execute(&pool)
    .await
    .unwrap();
    let (_, view) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={homograph}"),
        None,
    )
    .await;
    assert_eq!(view["total"], 0);
    let mut changed = saved["content"].clone();
    changed["sentence"]["level"] = json!("C1");
    let (status, updated) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":1,"context_entry_id":source,"context_sense_id":sense_id(source),"content":changed})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let (_, view) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={flower}"),
        None,
    )
    .await;
    assert_eq!(view["items"][0]["content"]["sentence"]["level"], "C1");
    let (status, _) = call(
        &state,
        actor,
        Method::DELETE,
        &format!("{ROOT}/{id}/associations/{source}"),
        Some(json!({"base_revision":1,"sense_id":sense_id(source)})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(
        &state,
        actor,
        Method::DELETE,
        &format!("{ROOT}/{id}/associations/{source}"),
        Some(json!({"base_revision":2,"sense_id":sense_id(source)})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, replay) = call(&state, actor, Method::POST, ROOT, Some(input.clone())).await;
    assert_eq!(replay["revision"], 3);
    assert_eq!(
        replay["entries"].as_array().unwrap().len(),
        1,
        "replay must not restore unlinked targets"
    );
    let (_, source_view) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={source}"),
        None,
    )
    .await;
    assert_eq!(source_view["total"], 0);
    let (_, flower_view) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={flower}"),
        None,
    )
    .await;
    assert_eq!(flower_view["total"], 1);
    let (status, _) = call(
        &state,
        actor,
        Method::DELETE,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":3})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&state, actor, Method::POST, ROOT, Some(input)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn rejects_invalid_targets_positions_and_foreign_draft_without_partial_writes(pool: PgPool) {
    let owner = admin(&pool).await;
    let other = admin(&pool).await;
    let source = entry(&pool, owner, "wonderful").await;
    let state = AppState::for_test(pool.clone());
    let original = content(source);
    let (status, _) = call(
        &state,
        other,
        Method::POST,
        ROOT,
        Some(
            json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":original}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let mut bad = original.clone();
    bad["annotations"][0]["source_segments"][0]["end"] = json!(99);
    let (status, _) = call(
        &state,
        owner,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":bad})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut missing = original.clone();
    missing["annotations"][0]["target"]["target_entry_id"] = json!(Uuid::now_v7());
    let (status, _) = call(
        &state,
        owner,
        Method::POST,
        ROOT,
        Some(
            json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":missing}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentences")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let input =
        json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":original});
    let (status, _) = call(&state, owner, Method::POST, ROOT, Some(input.clone())).await;
    assert_eq!(status, StatusCode::OK);
    let mut conflict = input;
    conflict["content"]["sentence"]["level"] = json!("C2");
    let (status, _) = call(&state, owner, Method::POST, ROOT, Some(conflict)).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn rejects_voice_settings_and_noncanonical_translation_annotations(pool: PgPool) {
    let owner = admin(&pool).await;
    let source = entry(&pool, owner, "wonderful").await;
    let state = AppState::for_test(pool.clone());
    let mut voice = content(source);
    voice["sentence"]["en_text"]["common"]["voice_profile"] =
        json!({"voices":[{"voice_id":"en-GB-SoniaNeural","enabled":true,"rate_percent":1000}]});
    let (status, _) = call(
        &state,
        owner,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":voice})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut alias = content(source);
    alias["sentence"]["zh_text"]["annotations"] =
        json!([{"type":"emphasis","start":0,"end":999,"level":"strong"}]);
    let (status, _) = call(
        &state,
        owner,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":alias})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentences")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn target_archive_race_rechecks_status_before_writing(pool: PgPool) {
    let owner = admin(&pool).await;
    let source = entry(&pool, owner, "wonderful").await;
    let target = entry(&pool, owner, "flower").await;
    let state = AppState::for_test(pool.clone());
    let mut archive = pool.begin().await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET archived_at=now() WHERE id=$1")
        .bind(target)
        .execute(&mut *archive)
        .await
        .unwrap();
    let mut proposed = content(source);
    proposed["annotations"][1]["target"] = json!(target_ref(target));
    let request = tokio::spawn(async move {
        call(
            &state,
            owner,
            Method::POST,
            ROOT,
            Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":proposed})),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%ORDER BY id FOR SHARE%')").fetch_one(&pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("target validation must hold a row lock");
    archive.commit().await.unwrap();
    let (status, _) = request.await.unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentences")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn origin_deletion_does_not_lock_or_remove_independently_published_sentence(pool: PgPool) {
    let owner = admin(&pool).await;
    let source = entry(&pool, owner, "wonderful").await;
    let state = AppState::for_test(pool.clone());
    let proposed = content(source);
    let (status, saved) = call(
        &state,
        owner,
        Method::POST,
        ROOT,
        Some(
            json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":proposed}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = Uuid::parse_str(saved["id"].as_str().unwrap()).unwrap();
    let mut unlinked = saved["content"].clone();
    unlinked["annotations"] = json!([]);
    let (status, _) = call(
        &state,
        owner,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":1,"content":unlinked})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut locked = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM lexicon.shared_sentences WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut *locked)
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        sqlx::query("DELETE FROM lexicon.entries WHERE id=$1")
            .bind(source)
            .execute(&pool),
    )
    .await
    .expect("origin deletion must not wait on sentence lock")
    .unwrap();
    locked.rollback().await.unwrap();
    let (status, remaining) = call(&state, owner, Method::GET, &format!("{ROOT}/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(remaining["entries"], json!([]));
}

#[sqlx::test]
async fn shared_schema_can_be_reverted_and_reapplied_in_isolation(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../migrations/20260912230000_shared_sentence_senses.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20260912181500_shared_sentence_origin.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20260912180000_shared_sentences.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20260912180000_shared_sentences.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20260912181500_shared_sentence_origin.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20260912230000_shared_sentence_senses.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentences")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn list_remains_consistent_while_sentences_are_deleted(pool: PgPool) {
    let owner = admin(&pool).await;
    let source = entry(&pool, owner, "wonderful").await;
    let state = AppState::for_test(pool.clone());
    let mut ids = Vec::new();
    for _ in 0..12 {
        let (status, item) = call(
            &state,
            owner,
            Method::POST,
            ROOT,
            Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":content(source)})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        ids.push(item["id"].as_str().unwrap().to_owned());
    }
    let reader_state = state.clone();
    let reader = tokio::spawn(async move {
        for _ in 0..20 {
            let (status, page) = call(
                &reader_state,
                owner,
                Method::GET,
                &format!("{ROOT}?page_size=50"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{page}");
            assert_eq!(
                page["total"].as_u64().unwrap() as usize,
                page["items"].as_array().unwrap().len()
            );
        }
    });
    for id in ids {
        let (status, _) = call(
            &state,
            owner,
            Method::DELETE,
            &format!("{ROOT}/{id}"),
            Some(json!({"base_revision":1})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    reader.await.unwrap();
}

#[sqlx::test]
async fn busy_entry_context_returns_retryable_conflict_without_writes(pool: PgPool) {
    let owner = admin(&pool).await;
    let source = entry(&pool, owner, "wonderful").await;
    let state = AppState::for_test(pool.clone());
    let mut held = pool.begin().await.unwrap();
    tsz_rust::lexicon::repository::LexiconRepository::lock_surface_contexts(&mut held, &[source])
        .await
        .unwrap();
    let (status, problem) = call(
        &state,
        owner,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":content(source)})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(problem["code"], "reference_conflict");
    held.rollback().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentences")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn rollback_refuses_to_erase_new_shared_content(pool: PgPool) {
    let owner = admin(&pool).await;
    let source = entry(&pool, owner, "wonderful").await;
    let state = AppState::for_test(pool.clone());
    let (status, _) = call(
        &state,
        owner,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":content(source)})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let error = sqlx::raw_sql(include_str!(
        "../migrations/20260912180000_shared_sentences.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("shared sentences exist"));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentences")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn shared_references_block_single_and_batch_archive_until_explicitly_unlinked(pool: PgPool) {
    let owner = admin(&pool).await;
    // Earlier rows would be archived first by the sorted batch if the transaction were not atomic.
    let plain = entry(&pool, owner, "plain").await;
    let source = entry(&pool, owner, "origin").await;
    let target = entry(&pool, owner, "wonderful").await;
    let redis_url = std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .expect("isolated Redis URL");
    let redis = deadpool_redis::Config::from_url(redis_url)
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(tsz_rust::config::SmartLexiconV3Flags::all_enabled());
    let (status, saved) = call(
        &state,
        owner,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":target,"source_sense_id":sense_id(target),"content":content(target)})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let sentence_id = saved["id"].as_str().unwrap();
    let archive_path = format!("/api/v1/admin/lexicon/entries/{target}/archive");
    let archive_input = json!({"base_revision":1,"base_lifecycle_revision":1});
    let (status, problem) = call(
        &state,
        owner,
        Method::POST,
        &archive_path,
        Some(archive_input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(problem["code"], "reference_conflict");
    let (status, problem) = call(&state, owner, Method::POST, "/api/v1/admin/lexicon/entries/archive-batch", Some(json!({"entries":[{"id":plain,"base_revision":1,"base_lifecycle_revision":1},{"id":source,"base_revision":1,"base_lifecycle_revision":1},{"id":target,"base_revision":1,"base_lifecycle_revision":1}]}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    let archived: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.entries WHERE archived_at IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        archived, 0,
        "batch including the source still cannot strand independently published sentences"
    );
    let mut updated_content = saved["content"].clone();
    updated_content["sentence"]["level"] = json!("C1");
    let (status, updated) = call(
        &state,
        owner,
        Method::PUT,
        &format!("{ROOT}/{sentence_id}"),
        Some(json!({"base_revision":1,"content":updated_content})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let (status, _) = call(
        &state,
        owner,
        Method::DELETE,
        &format!("{ROOT}/{sentence_id}/associations/{target}"),
        Some(json!({"base_revision":2,"sense_id":sense_id(target)})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, archived) = call(
        &state,
        owner,
        Method::POST,
        &archive_path,
        Some(archive_input),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    let (status, retained) = call(
        &state,
        owner,
        Method::GET,
        &format!("{ROOT}/{sentence_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retained["content"]["sentence"]["level"], "C1");
}

fn linked(target: Uuid, segments: Value) -> Value {
    json!({"id":Uuid::now_v7(),"source_dialect":"common","source_segments":segments,"target":target_ref(target)})
}
fn sentence_body(text: &str, annotations: Value) -> Value {
    let mut body = content(Uuid::now_v7());
    body["sentence"]["en_text"]["common"]["value"]["text"] = json!(text);
    body["annotations"] = annotations;
    body
}
async fn registered_form(pool: &PgPool, id: Uuid, surface: &str, kind: &str, dialect: &str) {
    let mut forms: Value =
        sqlx::query_scalar("SELECT forms FROM lexicon.entry_editor_projection WHERE entry_id=$1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
    forms["pos"][0]["forms"].as_array_mut().unwrap().push(json!({"id":node_id(id,6),"form_type":"past_tense","regional_variants":{"mode":"uk_us","uk":{"id":node_id(id,7),"dialect":"uk","spelling":surface,"origin":"manual","pronunciations":[]},"us":{"id":node_id(id,8),"dialect":"us","spelling":"unused","origin":"manual","pronunciations":[]}}}));
    forms["pos"][0]["form_groups"] = json!([{"id":node_id(id,5),"is_regular":true,"members":[{"id":node_id(id,9),"form_id":node_id(id,2)},{"id":node_id(id,10),"form_id":node_id(id,6)}]}]);
    sqlx::query("UPDATE lexicon.entry_editor_projection SET forms=$2 WHERE entry_id=$1")
        .bind(id)
        .bind(forms)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO lexicon.surface_sources(entry_id,source_id,source_kind,source_node_id,language,entry_kind,dialect,dialect_scope,surface,normalized_surface,normalization_version,source_revision,content_scope,pos_id,pos,form_type,variant_id,form_id,group_ids,projection_version) VALUES($1,$2,'form_variant',$3,'en',$4,$5,$5,$6,$6,1,1,'draft',$7,'verb','past_tense',$3,$8,ARRAY[$7]::uuid[],'test')")
        .bind(id).bind(Uuid::now_v7().to_string()).bind(node_id(id,7)).bind(kind).bind(dialect).bind(surface).bind(node_id(id,1)).bind(node_id(id,6)).execute(pool).await.unwrap();
}

#[sqlx::test]
async fn context_requires_exact_entry_and_complete_registered_surface(pool: PgPool) {
    let actor = admin(&pool).await;
    let phrase = entry(&pool, actor, "make up").await;
    sqlx::query("UPDATE lexicon.entries SET kind='phrase' WHERE id=$1")
        .bind(phrase)
        .execute(&pool)
        .await
        .unwrap();
    let homograph = entry(&pool, actor, "make up").await;
    sqlx::query("UPDATE lexicon.entries SET kind='phrase' WHERE id=$1")
        .bind(homograph)
        .execute(&pool)
        .await
        .unwrap();
    let state = AppState::for_test(pool.clone());
    let segments =
        json!([{"start":3,"end":7,"surface":"make"},{"start":18,"end":20,"surface":"up"}]);
    let text = "We make the story up.";
    for annotations in [
        json!([]),
        json!([linked(homograph, segments.clone())]),
        json!([{"id":Uuid::now_v7(),"source_dialect":"common","source_segments":segments,"target":{"state":"pending","kind":"phrase","headword":"make up","gloss":null}}]),
    ] {
        let body = sentence_body(text, annotations);
        let (status, problem) = call(
            &state,
            actor,
            Method::POST,
            ROOT,
            Some(
                json!({"source_entry_id":phrase,"source_sense_id":sense_id(phrase),"content":body}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    }
    let body = sentence_body(text, json!([linked(phrase, segments.clone())]));
    let (status, saved) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":phrase,"source_sense_id":sense_id(phrase),"content":body})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "noncontiguous lemma must work: {saved}"
    );
    let id = saved["id"].as_str().unwrap();
    let mut removed = saved["content"].clone();
    removed["annotations"] = json!([]);
    let (status, _) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":1,"context_entry_id":phrase,"context_sense_id":sense_id(phrase),"content":removed})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "contextual save cannot drop the last relation"
    );
    let (_, unchanged) = call(&state, actor, Method::GET, &format!("{ROOT}/{id}"), None).await;
    assert_eq!(unchanged["revision"], 1, "failed save is atomic");
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id=$1")
        .bind(phrase)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        revision, 1,
        "independent sentence save must not save meanings"
    );
    let (status, _) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":1,"content":removed})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "global editor may leave the sentence unlinked"
    );
    let mut substring = sentence_body(
        "We maker the story up.",
        json!([linked(
            phrase,
            json!([{"start":3,"end":7,"surface":"make"},{"start":19,"end":21,"surface":"up"}])
        )]),
    );
    let (status, _) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":phrase,"source_sense_id":sense_id(phrase),"content":substring})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "maker cannot be truncated to make"
    );
    substring = sentence_body(
        "We made the story up.",
        json!([linked(
            phrase,
            json!([{"start":3,"end":7,"surface":"made"},{"start":18,"end":20,"surface":"up"}])
        )]),
    );
    let (status, _) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":phrase,"source_sense_id":sense_id(phrase),"content":substring})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "do not guess unregistered inflections"
    );
    registered_form(&pool, phrase, "made up", "phrase", "uk").await;
    substring["annotations"][0]["target"]["target_form_id"] = json!(node_id(phrase, 6));
    substring["annotations"][0]["target"]["target_variant_id"] = json!(node_id(phrase, 7));
    let (status, saved_form) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":phrase,"source_sense_id":sense_id(phrase),"content":substring})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "registered form must work: {saved_form}"
    );
    let mut wrong_form = body.clone();
    wrong_form["sentence"]["id"] = json!(Uuid::now_v7());
    wrong_form["annotations"][0]["target"]["target_form_id"] = json!(node_id(phrase, 6));
    wrong_form["annotations"][0]["target"]["target_variant_id"] = json!(node_id(phrase, 7));
    let (status, _) = call(&state, actor, Method::POST, ROOT,
        Some(json!({"source_entry_id":phrase,"source_sense_id":sense_id(phrase),"content":wrong_form}))).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "lemma text cannot select the past-tense form"
    );
    substring["sentence"]["id"] = json!(Uuid::now_v7());
    substring["annotations"][0]["source_dialect"] = json!("us");
    let variant = substring["sentence"]["en_text"]["common"].clone();
    substring["sentence"]["en_text"] =
        json!({"mode":"uk_us","uk":{"state":"empty"},"us":{"state":"ready","variant":variant}});
    let (status, _) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":phrase,"source_sense_id":sense_id(phrase),"content":substring})),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "UK-only form must not validate a US annotation"
    );
}

#[sqlx::test]
async fn repeated_links_count_once_and_all_are_removed_by_explicit_unlink(pool: PgPool) {
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "make").await;
    let state = AppState::for_test(pool.clone());
    let proposed = sentence_body(
        "make make",
        json!([
            linked(source, json!([{"start":0,"end":4,"surface":"make"}])),
            linked(source, json!([{"start":5,"end":9,"surface":"make"}]))
        ]),
    );
    let (status, saved) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(
            json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":proposed}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let id = saved["id"].as_str().unwrap();
    let (_, list) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={source}&page_size=1"),
        None,
    )
    .await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (status, _) = call(
        &state,
        actor,
        Method::DELETE,
        &format!("{ROOT}/{id}/associations/{source}"),
        Some(json!({"base_revision":1,"sense_id":sense_id(source)})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, remaining) = call(&state, actor, Method::GET, &format!("{ROOT}/{id}"), None).await;
    assert_eq!(remaining["content"]["annotations"], json!([]));
    assert_eq!(
        remaining["content"]["sentence"],
        saved["content"]["sentence"]
    );
    let (_, list) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={source}"),
        None,
    )
    .await;
    assert_eq!(list["total"], 0);
}

#[sqlx::test]
async fn exact_targets_are_paginated_and_use_the_same_forms_as_save(pool: PgPool) {
    let actor = admin(&pool).await;
    let other = admin(&pool).await;
    let context = entry(&pool, actor, "make").await;
    for _ in 0..51 {
        entry(&pool, actor, "make").await;
    }
    entry(&pool, actor, "maker").await;
    entry(&pool, other, "make").await;
    let state = AppState::for_test(pool.clone());
    let (status, page) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}/targets?q=make&kind=word&dialect=common&context_entry_id={context}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 53);
    assert_eq!(page["items"].as_array().unwrap().len(), 50);
    let (_, page2) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}/targets?q=make&kind=word&page=2"),
        None,
    )
    .await;
    assert_eq!(page2["total"], 53);
    assert_eq!(page2["items"].as_array().unwrap().len(), 3);
    let (_, target) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}/targets?entry_id={context}&context_entry_id={context}"),
        None,
    )
    .await;
    assert_eq!(target["items"][0]["id"], context.to_string());
    assert_eq!(target["items"][0]["surfaces"].as_array().unwrap().len(), 2);
    let mut tampered = content(context);
    tampered["annotations"][1]["target"]["headword"] = json!("make1");
    let (status, _) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":context,"source_sense_id":sense_id(context),"content":tampered})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "pending text cannot be changed independently of the selected fragment"
    );
}

#[sqlx::test]
async fn global_editor_can_repair_archived_legacy_targets(pool: PgPool) {
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let flower = entry(&pool, actor, "flower").await;
    let state = AppState::for_test(pool.clone());
    let mut body = content(source);
    body["annotations"][1]["target"] = json!(target_ref(flower));
    let (status, saved) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":body})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    sqlx::query("UPDATE lexicon.entries SET archived_at=now() WHERE id=$1")
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    body["annotations"] = json!([body["annotations"][1].clone()]);
    let id = saved["id"].as_str().unwrap();
    let (status, repaired) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":1,"content":body})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repaired}");
    assert_eq!(repaired["entries"].as_array().unwrap().len(), 1);
    assert_eq!(repaired["entries"][0]["id"], flower.to_string());
    assert_eq!(
        repaired["content"]["sentence"],
        saved["content"]["sentence"]
    );
}

#[sqlx::test]
async fn shared_read_and_global_edit_do_not_require_ownership_of_linked_drafts(pool: PgPool) {
    let owner = admin(&pool).await;
    let other = admin(&pool).await;
    let source = entry(&pool, owner, "wonderful").await;
    let state = AppState::for_test(pool.clone());
    let (status, saved) = call(
        &state,
        owner,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":content(source)})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, list) = call(
        &state,
        other,
        Method::GET,
        &format!("{ROOT}?entry_id={source}"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a filtered shared-library read is not a word edit"
    );
    assert_eq!(list["items"][0]["id"], saved["id"]);
    let id = saved["id"].as_str().unwrap();
    let mut changed = saved["content"].clone();
    changed["sentence"]["level"] = json!("C1");
    let (status, updated) = call(
        &state,
        other,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":1,"content":changed})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "active admins can edit the independently shared entity: {updated}"
    );
    let (status, _) = call(
        &state,
        other,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":2,"context_entry_id":source,"context_sense_id":sense_id(source),"content":changed})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the contextual word editor still respects word write permission"
    );
}

#[sqlx::test]
async fn sense_membership_is_precise_and_unlink_preserves_other_senses(pool: PgPool) {
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "make").await;
    let second = node_id(source, 11);
    let mut meanings: Value = sqlx::query_scalar(
        "SELECT meanings FROM lexicon.entry_editor_projection WHERE entry_id=$1",
    )
    .bind(source)
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut other = meanings["pos"][0]["senses"][0].clone();
    other["id"] = json!(second);
    meanings["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .push(other);
    sqlx::query("UPDATE lexicon.entry_editor_projection SET meanings=$2 WHERE entry_id=$1")
        .bind(source)
        .bind(meanings)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO lexicon.nodes(id,entry_id,node_type) VALUES($1,$2,'sense')")
        .bind(second)
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    let state = AppState::for_test(pool.clone());
    let mut second_link = linked(source, json!([{"start":5,"end":9,"surface":"make"}]));
    second_link["target"]["target_sense_id"] = json!(second);
    let content = sentence_body(
        "make make",
        json!([
            linked(source, json!([{"start":0,"end":4,"surface":"make"}])),
            second_link
        ]),
    );
    let (status, saved) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(
            json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":content}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let id = saved["id"].as_str().unwrap();
    assert_eq!(saved["entries"][0]["senses"].as_array().unwrap().len(), 2);
    for sense in [sense_id(source), second] {
        let (_, list) = call(
            &state,
            actor,
            Method::GET,
            &format!("{ROOT}?entry_id={source}&sense_id={sense}"),
            None,
        )
        .await;
        assert_eq!(list["total"], 1);
        assert_eq!(list["items"][0]["id"], id);
    }
    let (status, _) = call(
        &state,
        actor,
        Method::DELETE,
        &format!("{ROOT}/{id}/associations/{source}"),
        Some(json!({"base_revision":1,"sense_id":sense_id(source)})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, first) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={source}&sense_id={}", sense_id(source)),
        None,
    )
    .await;
    let (_, other) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={source}&sense_id={second}"),
        None,
    )
    .await;
    assert_eq!(first["total"], 0);
    assert_eq!(other["total"], 1);
    assert_eq!(
        other["items"][0]["content"]["sentence"],
        saved["content"]["sentence"]
    );
    let (status, _) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?sense_id={second}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status,_)=call(&state,actor,Method::PUT,&format!("{ROOT}/{id}"),Some(json!({"base_revision":2,"context_entry_id":source,"context_sense_id":sense_id(source),"content":other["items"][0]["content"]}))).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "another sense of the same entry cannot satisfy the editing context"
    );
}

#[sqlx::test]
async fn sense_identity_cannot_be_forged_and_legacy_links_require_repair(pool: PgPool) {
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let other = entry(&pool, actor, "wonderful").await;
    let state = AppState::for_test(pool.clone());
    for field in [
        "target_pos_id",
        "target_base_form_id",
        "target_form_id",
        "target_variant_id",
        "target_sense_id",
    ] {
        let mut body = content(source);
        body["annotations"][0]["target"][field] = target_ref(other)[field].clone();
        let (status, problem) = call(
            &state,
            actor,
            Method::POST,
            ROOT,
            Some(
                json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":body}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {problem}");
    }
    let mut body = content(source);
    body["annotations"][0]["target"]["target_publication_id"] = json!(Uuid::now_v7());
    let (status, _) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":body})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"content":content(source)})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unsaved sense cannot create a sentence"
    );
    let (status,saved)=call(&state,actor,Method::POST,ROOT,Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":content(source)}))).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let id = Uuid::parse_str(saved["id"].as_str().unwrap()).unwrap();
    sqlx::query("UPDATE lexicon.shared_sentence_annotations SET target_ref=NULL,target_sense_id=NULL WHERE sentence_id=$1 AND target_entry_id=$2").bind(id).bind(source).execute(&pool).await.unwrap();
    let (_, legacy) = call(&state, actor, Method::GET, &format!("{ROOT}/{id}"), None).await;
    assert_eq!(
        legacy["content"]["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["target"]["target_entry_id"] == source.to_string())
            .unwrap()["target"]["state"],
        "entry_only"
    );
    let (_, list) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={source}&sense_id={}", sense_id(source)),
        None,
    )
    .await;
    assert_eq!(
        list["total"], 0,
        "legacy entry links must not be assigned to a guessed sense"
    );
    let (status, _) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":1,"content":legacy["content"]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, repaired) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":1,"content":saved["content"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repaired}");
    assert_eq!(repaired["id"], saved["id"]);
    let error = sqlx::raw_sql(include_str!(
        "../migrations/20260913120000_shared_sentence_targets.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("sense targets"), "{error}");
}

#[sqlx::test]
async fn referenced_sense_cannot_be_removed_until_unlinked(pool: PgPool) {
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let state = AppState::for_test(pool.clone())
        .with_smart_lexicon_v3_flags_for_test(tsz_rust::config::SmartLexiconV3Flags::all_enabled());
    let (_,saved)=call(&state,actor,Method::POST,ROOT,Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":content(source)}))).await;
    let id = saved["id"].as_str().unwrap();
    let path = format!("/api/v1/admin/lexicon/entries/{source}/steps/meanings");
    let input = json!({"schema_version":3,"base_revision":1,"intent":"save","content":{"sense_groups":[],"pos":[{"pos_id":node_id(source,1),"grammar_structures":[],"senses":[]}]}});
    let (status, problem) = call(&state, actor, Method::PUT, &path, Some(input.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(problem["code"], "reference_conflict");
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id=$1")
        .bind(source)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(revision, 1);
    let (status, _) = call(
        &state,
        actor,
        Method::DELETE,
        &format!("{ROOT}/{id}/associations/{source}"),
        Some(json!({"base_revision":1,"sense_id":sense_id(source)})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, result) = call(&state, actor, Method::PUT, &path, Some(input)).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let (status,_)=call(&state,actor,Method::POST,ROOT,Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":content(source)}))).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "retained node UUID does not make a removed sense valid"
    );
}

#[sqlx::test]
async fn form_changes_and_old_publications_cannot_strand_sentence_targets(pool: PgPool) {
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let redis_url = std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .expect("isolated Redis URL");
    let redis = deadpool_redis::Config::from_url(redis_url)
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(tsz_rust::config::SmartLexiconV3Flags::all_enabled());
    let (status, envelope) = call(
        &state,
        actor,
        Method::GET,
        &format!("/api/v1/admin/lexicon/entries/{source}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{envelope}");
    let mut snapshot = envelope["word"].clone();
    let publication = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) VALUES($1,$2,1,1,3,$3,$4,$5)").bind(publication).bind(source).bind(&snapshot).bind(vec![1u8;32]).bind(actor).execute(&pool).await.unwrap();
    let mut body = content(source);
    body["annotations"][0]["target"]["target_publication_id"] = json!(publication);
    let (status, saved) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":body})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let id = saved["id"].as_str().unwrap();
    for mutate in ["spelling", "variant"] {
        let mut forms = snapshot["forms"].clone();
        let field = if mutate == "spelling" {
            "spelling"
        } else {
            "id"
        };
        forms["pos"][0]["forms"][0]["regional_variants"]["common"][field] = if mutate == "spelling"
        {
            json!("wonderfully")
        } else {
            json!(Uuid::now_v7())
        };
        let (status, impact) = call(
            &state,
            actor,
            Method::POST,
            &format!("/api/v1/admin/lexicon/entries/{source}/steps/forms/impact"),
            Some(json!({"schema_version":3,"base_revision":1,"content":forms})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{impact}");
        let (status, problem) = call(
            &state,
            actor,
            Method::PUT,
            &format!("/api/v1/admin/lexicon/entries/{source}/steps/forms"),
            Some(json!({"schema_version":3,"base_revision":1,"intent":"save","content":forms,"confirmed_impact_token":impact["confirmation_token"]})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{mutate}: {problem}");
        assert_eq!(problem["code"], "reference_conflict");
    }
    // A historical version without the referenced sense cannot become current.
    snapshot["meanings"]["pos"][0]["senses"] = json!([]);
    let empty_publication = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) VALUES($1,$2,2,2,3,$3,$4,$5)").bind(empty_publication).bind(source).bind(&snapshot).bind(vec![2u8;32]).bind(actor).execute(&pool).await.unwrap();
    let (status, problem) = call(
        &state,
        actor,
        Method::POST,
        &format!(
            "/api/v1/admin/lexicon/entries/{source}/publications/{empty_publication}/activate"
        ),
        Some(json!({"schema_version":3,"base_revision":1,"base_lifecycle_revision":1})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(problem["code"], "reference_conflict");
    let (_, current) = call(
        &state,
        actor,
        Method::GET,
        &format!("/api/v1/admin/lexicon/entries/{source}"),
        None,
    )
    .await;
    assert_eq!(current["word"]["revision"], 1);
    let (status, _) = call(
        &state,
        actor,
        Method::DELETE,
        &format!("{ROOT}/{id}/associations/{source}"),
        Some(json!({"base_revision":1,"sense_id":sense_id(source)})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status,removed)=call(&state,actor,Method::PUT,&format!("/api/v1/admin/lexicon/entries/{source}/steps/meanings"),Some(json!({"schema_version":3,"base_revision":1,"intent":"save","content":snapshot["meanings"]}))).await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    // The old valid snapshot still contains the sense, but it no longer exists in saved draft.
    body["sentence"]["id"] = json!(Uuid::now_v7());
    let (status, problem) = call(
        &state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":body})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "historical snapshot cannot resurrect a removed sense: {problem}"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentences")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "rejected reference must not create a partial sentence"
    );
}

#[sqlx::test]
async fn pending_candidates_are_paginated_and_claim_preserves_sentence(pool: PgPool) {
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let flower = entry(&pool, actor, "flower").await;
    let state = AppState::for_test(pool.clone());
    let mut first = content(source);
    first["sentence"]["en_text"]["common"]["value"]["text"] = json!("A wonderful flower flower.");
    let mut repeated = first["annotations"][1].clone();
    repeated["id"] = json!(Uuid::now_v7());
    repeated["source_segments"] = json!([{"start":19,"end":25,"surface":"flower"}]);
    first["annotations"].as_array_mut().unwrap().push(repeated);
    let mut saved = Vec::new();
    for body in [first, content(source)] {
        let (status, item) = call(
            &state,
            actor,
            Method::POST,
            ROOT,
            Some(
                json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":body}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{item}");
        saved.push(item);
    }
    for page in [1, 2] {
        let (status, items) = call(
            &state,
            actor,
            Method::GET,
            &format!("{ROOT}?pending_entry_id={flower}&page_size=1&page={page}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{items}");
        assert_eq!(
            items["total"], 2,
            "one sentence with repeated pending must count once"
        );
        assert_eq!(items["items"].as_array().unwrap().len(), 1);
    }
    let original = &saved[0];
    let id = original["id"].as_str().unwrap();
    let mut changed = original["content"].clone();
    for annotation in changed["annotations"].as_array_mut().unwrap() {
        if annotation["target"]["state"] == "pending" {
            annotation["target"] = target_ref(flower);
        }
    }
    let input = json!({"base_revision":1,"context_entry_id":flower,"context_sense_id":sense_id(flower),"content":changed});
    let (status, claimed) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(claimed["id"], original["id"]);
    assert_eq!(
        claimed["content"]["sentence"],
        original["content"]["sentence"]
    );
    assert_eq!(claimed["content"], changed);
    let (_, candidates) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?pending_entry_id={flower}"),
        None,
    )
    .await;
    assert_eq!(candidates["total"], 1);
    assert_eq!(candidates["items"][0]["id"], saved[1]["id"]);
    let (_, linked) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={flower}&sense_id={}", sense_id(flower)),
        None,
    )
    .await;
    assert_eq!(linked["total"], 1);
    let (status, _) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // Sorting uses updated_at rather than creation order.
    sqlx::query("UPDATE lexicon.shared_sentences SET updated_at='2099-01-01' WHERE id=$1")
        .bind(Uuid::parse_str(id).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let (_, sorted) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?sort=updated_at_desc&page_size=1"),
        None,
    )
    .await;
    assert_eq!(sorted["items"][0]["id"], original["id"]);
}

#[sqlx::test]
async fn sentence_status_filters_and_pending_scope_are_precise(pool: PgPool) {
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let flower = entry(&pool, actor, "flower").await;
    let state = AppState::for_test(pool.clone());
    let (_, original) = call(&state, actor, Method::POST, ROOT, Some(json!({"source_entry_id":source,"source_sense_id":sense_id(source),"content":content(source)}))).await;
    let id = original["id"].as_str().unwrap();
    for (status, total) in [("pending", 1), ("entry_only", 0), ("unlinked", 0)] {
        let (_, items) = call(
            &state,
            actor,
            Method::GET,
            &format!("{ROOT}?association_status={status}"),
            None,
        )
        .await;
        assert_eq!(items["total"], total, "{status}: {items}");
    }
    // A skeleton without senses may discover candidates, but cannot claim a made-up sense.
    sqlx::query("UPDATE lexicon.entry_editor_projection SET meanings=jsonb_set(meanings,'{pos,0,senses}','[]') WHERE entry_id=$1").bind(flower).execute(&pool).await.unwrap();
    let (_, items) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?pending_entry_id={flower}"),
        None,
    )
    .await;
    assert_eq!(items["total"], 1);
    let mut changed = original["content"].clone();
    changed["annotations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|a| a["target"]["state"] == "pending")
        .unwrap()["target"] = target_ref(flower);
    let (status, _) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":1,"content":changed})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Current forms cannot resurrect the stale initial lemma after a rename.
    sqlx::query("UPDATE lexicon.surface_sources SET surface='flowers',normalized_surface='flowers' WHERE entry_id=$1").bind(flower).execute(&pool).await.unwrap();
    let (_, items) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?pending_entry_id={flower}"),
        None,
    )
    .await;
    assert_eq!(items["total"], 0);
    sqlx::query("UPDATE lexicon.surface_sources SET surface='flower',normalized_surface='flower',dialect_scope='us' WHERE entry_id=$1").bind(flower).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.shared_sentence_annotations SET source_dialect='uk' WHERE sentence_id=$1 AND pending_kind IS NOT NULL").bind(Uuid::parse_str(id).unwrap()).execute(&pool).await.unwrap();
    let (_, items) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?pending_entry_id={flower}"),
        None,
    )
    .await;
    assert_eq!(
        items["total"], 0,
        "UK pending cannot match US-only spelling"
    );
    sqlx::query("UPDATE lexicon.shared_sentence_annotations SET source_dialect='common',pending_kind='phrase' WHERE sentence_id=$1 AND pending_kind IS NOT NULL").bind(Uuid::parse_str(id).unwrap()).execute(&pool).await.unwrap();
    let (_, items) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?pending_entry_id={flower}"),
        None,
    )
    .await;
    assert_eq!(items["total"], 0, "word and phrase must not cross-match");
    // Historical entry-only links have their own status, distinct from pending.
    sqlx::query("UPDATE lexicon.shared_sentence_annotations SET target_sense_id=NULL,target_ref=NULL WHERE sentence_id=$1 AND target_entry_id IS NOT NULL").bind(Uuid::parse_str(id).unwrap()).execute(&pool).await.unwrap();
    let (_, items) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?association_status=entry_only"),
        None,
    )
    .await;
    assert_eq!(items["total"], 1);
    sqlx::query("DELETE FROM lexicon.shared_sentence_annotations WHERE sentence_id=$1 AND target_entry_id IS NOT NULL").bind(Uuid::parse_str(id).unwrap()).execute(&pool).await.unwrap();
    let (_, items) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?association_status=unlinked"),
        None,
    )
    .await;
    assert_eq!(items["total"], 1, "pending-only sentence is unlinked");
    for query in [
        format!("entry_id={source}&pending_entry_id={flower}"),
        "association_status=unknown".into(),
        "sort=unknown".into(),
    ] {
        let (status, _) = call(&state, actor, Method::GET, &format!("{ROOT}?{query}"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

#[test]
fn sentence_list_openapi_parameters_are_optional_queries() {
    use utoipa::OpenApi;
    let spec = serde_json::to_value(tsz_rust::openapi::ApiDoc::openapi()).unwrap();
    let parameters = spec["paths"][ROOT]["get"]["parameters"].as_array().unwrap();
    for name in [
        "association_status",
        "pending_entry_id",
        "sort",
        "entry_id",
        "sense_id",
        "page",
        "page_size",
    ] {
        let parameter = parameters.iter().find(|p| p["name"] == name).unwrap();
        assert_eq!(parameter["in"], "query", "{name}: {parameter}");
        assert_eq!(parameter["required"], false, "{name}: {parameter}");
    }
}
