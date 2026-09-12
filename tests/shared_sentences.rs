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
async fn entry(pool: &PgPool, owner: Uuid, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,revision,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word',1,'{}',$2,$2)").bind(id).bind(owner).execute(pool).await.unwrap();
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
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
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
    json!({"sentence":{"id":Uuid::now_v7(),"level":"B1","en_text":{"mode":"unified","common":{"id":Uuid::now_v7(),"origin":"manual","value":{"version":2,"text":"A wonderful flower.","annotations":[]}}},"zh_text_id":translation,"zh_text":{"version":2,"text":"一朵美丽的花。","annotations":[]},"zh_translations":[{"id":translation,"band":"balanced_fluency","language":"zh","content":{"version":2,"text":"一朵美丽的花。","annotations":[]}}],"links":[]},"annotations":[{"id":Uuid::now_v7(),"source_dialect":"common","source_segments":[{"start":2,"end":11,"surface":"wonderful"}],"target":{"state":"linked","target_entry_id":target}},{"id":Uuid::now_v7(),"source_dialect":"common","source_segments":[{"start":12,"end":18,"surface":"flower"}],"target":{"state":"pending","kind":"word","headword":"flower","gloss":"花"}}]})
}

#[sqlx::test]
async fn independently_published_shared_content_candidates_and_deletion(pool: PgPool) {
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let flower = entry(&pool, actor, "flower").await;
    let other_flower = entry(&pool, actor, "flower").await;
    let state = AppState::for_test(pool.clone());
    let content = content(source);
    let id = content["sentence"]["id"].as_str().unwrap();
    let input = json!({"source_entry_id":source,"content":content});
    let (status, saved) = call(&state, actor, Method::POST, ROOT, Some(input.clone())).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["entries"][0]["collected"], true);
    let (_, replayed) = call(&state, actor, Method::POST, ROOT, Some(input.clone())).await;
    assert_eq!(replayed["revision"], 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentences")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let (_, candidates) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={flower}&candidates=true"),
        None,
    )
    .await;
    assert_eq!(candidates["total"], 1);
    let (_, other_candidates) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={other_flower}&candidates=true"),
        None,
    )
    .await;
    assert_eq!(other_candidates["total"], 1);
    let (_, before) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={flower}"),
        None,
    )
    .await;
    assert_eq!(before["total"], 0);
    let pending_id = content["annotations"][1]["id"].clone();
    let (status, collected) = call(
        &state,
        actor,
        Method::POST,
        &format!("{ROOT}/{id}/collections"),
        Some(json!({"base_revision":1,"entry_id":flower,"annotation_ids":[pending_id]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{collected}");
    assert_eq!(collected["revision"], 2);
    assert_eq!(
        collected["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["collected"] == true)
            .count(),
        2
    );
    let (_, other) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}?entry_id={other_flower}&candidates=true"),
        None,
    )
    .await;
    assert_eq!(other["total"], 0, "must not bind homographs automatically");
    let mut changed = collected["content"].clone();
    changed["sentence"]["level"] = json!("C1");
    let (status, updated) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":2,"content":changed})),
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
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":2})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(
        &state,
        actor,
        Method::DELETE,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":3})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for entry in [source, flower] {
        let (_, view) = call(
            &state,
            actor,
            Method::GET,
            &format!("{ROOT}?entry_id={entry}"),
            None,
        )
        .await;
        assert_eq!(view["total"], 0);
    }
    let (status, _) = call(&state, actor, Method::POST, ROOT, Some(input)).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "retry cannot resurrect deleted sentence"
    );
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
        Some(json!({"source_entry_id":source,"content":original})),
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
        Some(json!({"source_entry_id":source,"content":bad})),
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
        Some(json!({"source_entry_id":source,"content":missing})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentences")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let input = json!({"source_entry_id":source,"content":original});
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
        Some(json!({"source_entry_id":source,"content":voice})),
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
        Some(json!({"source_entry_id":source,"content":alias})),
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
    let mut proposed = content(target);
    proposed["annotations"][1]["target"]["headword"] = json!("bloom");
    let request = tokio::spawn(async move {
        call(
            &state,
            owner,
            Method::POST,
            ROOT,
            Some(json!({"source_entry_id":source,"content":proposed})),
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
    let mut proposed = content(source);
    proposed["annotations"] = json!([]);
    let (status, saved) = call(
        &state,
        owner,
        Method::POST,
        ROOT,
        Some(json!({"source_entry_id":source,"content":proposed})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = Uuid::parse_str(saved["id"].as_str().unwrap()).unwrap();
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
            Some(json!({"source_entry_id":source,"content":content(source)})),
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
        Some(json!({"source_entry_id":source,"content":content(source)})),
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
        Some(json!({"source_entry_id":source,"content":content(source)})),
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
