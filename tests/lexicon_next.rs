//! Real PostgreSQL + real authenticated router; no mocked repository or persistence.
use axum::{
    body::Body,
    http::{Request, StatusCode},
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
const ROOT: &str = "/api/v1/admin/lexicon-next";
async fn admin(pool: &PgPool, permissions: &[&str]) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("next-{}", id.simple()),
            display_name: "本批隔离测试".into(),
            password_hash: "test-only".into(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    for permission in permissions {
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,$2,$1)")
            .bind(id).bind(permission).execute(pool).await.unwrap();
    }
    id
}
async fn call(
    state: &AppState,
    actor: Option<Uuid>,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    call_with_groups(state, actor, method, path, body, false).await
}

async fn call_annotation_rule(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(format!("{ROOT}{path}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "Bearer {}",
                state.admin_token_manager.generate(actor, "admin").unwrap()
            ),
        )
        .header("x-tsz-next-forms", "v1")
        .header("x-tsz-next-annotation-rule", "v1")
        .header("x-tsz-next-annotation-kind", "word")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = tsz_rust::router(state.clone()).oneshot(req).await.unwrap();
    let status = response.status();
    let data =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, data)
}

async fn call_associations(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(format!("{ROOT}{path}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "Bearer {}",
                state.admin_token_manager.generate(actor, "admin").unwrap()
            ),
        )
        .header("x-tsz-next-forms", "v1")
        .header("x-tsz-next-sentence-associations", "v1")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = tsz_rust::router(state.clone()).oneshot(req).await.unwrap();
    let status = response.status();
    let data =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, data)
}
async fn call_with_groups(
    state: &AppState,
    actor: Option<Uuid>,
    method: &str,
    path: &str,
    body: Value,
    groups: bool,
) -> (StatusCode, Value) {
    call_client(state, actor, method, path, body, (groups, false, false)).await
}
async fn call_scoped(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    call_client(state, Some(actor), method, path, body, (true, true, false)).await
}
async fn call_client(
    state: &AppState,
    actor: Option<Uuid>,
    method: &str,
    path: &str,
    body: Value,
    capabilities: (bool, bool, bool),
) -> (StatusCode, Value) {
    call_audio_client(state, actor, method, path, body, capabilities, false).await
}
async fn call_audio_client(
    state: &AppState,
    actor: Option<Uuid>,
    method: &str,
    path: &str,
    body: Value,
    capabilities: (bool, bool, bool),
    audio: bool,
) -> (StatusCode, Value) {
    call_editor_client(
        state,
        actor,
        method,
        path,
        body,
        capabilities,
        (audio, false),
    )
    .await
}
async fn call_editor_client(
    state: &AppState,
    actor: Option<Uuid>,
    method: &str,
    path: &str,
    body: Value,
    capabilities: (bool, bool, bool),
    extensions: (bool, bool),
) -> (StatusCode, Value) {
    let (audio, metadata) = extensions;
    let (groups, ownership, grammar) = capabilities;
    let mut req = Request::builder()
        .method(method)
        .uri(format!("{ROOT}{path}"))
        .header("content-type", "application/json")
        .header("x-tsz-next-forms", "v1");
    if metadata {
        req = req.header("x-tsz-next-sense-metadata", "v1");
    }
    if audio {
        req = req.header("x-tsz-next-grammar-audio", "v1");
    }
    if grammar {
        req = req.header("x-tsz-next-grammar", "v1");
    }
    if groups {
        req = req.header("x-tsz-next-sense-groups", "v1");
    }
    if ownership {
        req = req.header("x-tsz-next-medium-ownership", "v1");
    }
    if let Some(actor) = actor {
        req = req.header(
            "authorization",
            format!(
                "Bearer {}",
                state.admin_token_manager.generate(actor, "admin").unwrap()
            ),
        );
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let data =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, data)
}
async fn create(state: &AppState, actor: Uuid) -> Value {
    let (status, entry) = call(
        state,
        Some(actor),
        "POST",
        "/entries",
        json!({"target_language":"en", "kind":"word", "headword":"centre"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{entry}");
    entry
}
fn draft() -> Value {
    json!({"schema_version":1,"headword":"centre","forms":[
        {"id":Uuid::now_v7(),"locale":"en-GB","text":"centre"},
        {"id":Uuid::now_v7(),"locale":"en-US","text":"center"}
    ],"senses":[{"id":Uuid::now_v7(),"part_of_speech":"noun","definitions":[
        {"id":Uuid::now_v7(),"locale":"en-GB","style":"definition","text":"the middle of something"},
        {"id":Uuid::now_v7(),"locale":"en-US","style":"sentence","text":"The center is the middle."},
        {"id":Uuid::now_v7(),"locale":"zh-Hans","style":"definition","text":"某物的中心位置"},
        {"id":Uuid::now_v7(),"locale":"zh-Hant","style":"definition","text":"某物的中心位置（繁體說明）"}
    ],"equivalents":[
        {"id":Uuid::now_v7(),"locale":"zh-Hans","text":"中心"},
        {"id":Uuid::now_v7(),"locale":"zh-Hant","text":"中心處"}
    ]}]})
}
const EDIT: &[&str] = &["words.access", "words.create", "words.edit"];

#[sqlx::test]
async fn creation_matches_current_forms_and_empty_drafts(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let reader = admin(&pool, &["words.access"]).await;
    let state = AppState::for_test(pool.clone());
    let query = json!({"target_language":"en","surface":"Center","page":1,"page_size":2});
    assert_eq!(
        call(&state, None, "POST", "/entry-matches", query.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &state,
            Some(reader),
            "POST",
            "/entry-matches",
            query.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut entries = Vec::new();
    for headword in [
        "ＣＥＮＴＥＲ",
        "former",
        "center",
        "centerpiece",
        "Center",
        "beautiful",
    ] {
        let (status, entry) = call(
            &state,
            Some(actor),
            "POST",
            "/entries",
            json!({"target_language":"en","kind":"word","headword":headword}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{entry}");
        entries.push(entry);
    }
    for (index, spellings) in [
        (1, vec!["center", "CENTER"]),
        (2, vec!["elsewhere"]),
        (5, vec!["more beautiful"]),
    ] {
        let mut content = draft();
        content["headword"] = entries[index]["draft"]["headword"].clone();
        content["forms"]=json!(spellings.into_iter().enumerate().map(|(index,text)|json!({"id":Uuid::now_v7(),"locale":if index==0 {"en-US"} else {"en-GB"},"text":text})).collect::<Vec<_>>());
        let (status, saved) = call(
            &state,
            Some(actor),
            "PUT",
            &format!("/entries/{}", entries[index]["id"].as_str().unwrap()),
            json!({"expected_revision":1,"draft":content}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
    }
    sqlx::query("UPDATE lexicon_next.entries SET status='archived',lifecycle_revision=lifecycle_revision+1 WHERE id=$1")
        .bind(entries[4]["id"].as_str().unwrap().parse::<Uuid>().unwrap()).execute(&pool).await.unwrap();
    let (status, first) = call(&state, Some(actor), "POST", "/entry-matches", query.clone()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["surface"], "Center");
    assert_eq!(first["pagination"]["total"], 2);
    assert_eq!(first["pagination"]["total_pages"], 1);
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert!(
        first["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["status"] == "draft")
    );
    let ids = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        ids,
        std::collections::BTreeSet::from([
            entries[0]["id"].as_str().unwrap(),
            entries[1]["id"].as_str().unwrap(),
        ])
    );
    let current = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == entries[1]["id"])
        .unwrap();
    assert_eq!(current["forms"].as_array().unwrap().len(), 2);
    assert_eq!(current["part_of_speech"], json!(["noun"]));
    assert_eq!(current["gloss"], "某物的中心位置");
    let (status, phrase) = call(
        &state,
        Some(actor),
        "POST",
        "/entry-matches",
        json!({"target_language":"en","surface":"MORE BEAUTIFUL"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{phrase}");
    assert_eq!(phrase["items"][0]["id"], entries[5]["id"]);
    assert_eq!(phrase["items"][0]["kind"], "word");
    let (status, empty) = call(
        &state,
        Some(actor),
        "POST",
        "/entry-matches",
        json!({"target_language":"en","surface":"cent"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["pagination"]["total"], 0);
    assert_eq!(empty["items"], json!([]));
    for invalid in [
        json!({"target_language":"en","surface":"center","page":0}),
        json!({"target_language":"en","surface":"center","page_size":101}),
        json!({"target_language":"fr","surface":"center"}),
        json!({"target_language":"en","surface":"中文"}),
    ] {
        assert_eq!(
            call(&state, Some(actor), "POST", "/entry-matches", invalid)
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon_next.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 6, "matching never writes an entry");
}

#[sqlx::test]
async fn detection_returns_all_source_records_without_creating_or_prefilling(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let reader = admin(&pool, &["words.access"]).await;
    let state = AppState::for_test(pool.clone());
    let input = json!({"target_language":"en","surface":"  Colour  "});
    assert_eq!(
        call(&state, None, "POST", "/detections", input.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&state, Some(reader), "POST", "/detections", input.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let unavailable = call(&state, Some(actor), "POST", "/detections", input.clone()).await;
    assert_eq!(
        unavailable.0,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        unavailable.1
    );
    let dataset: i64 = sqlx::query_scalar("INSERT INTO dictionary.datasets(version,source_name,source_version,rules_version,terms_sha256,regions_sha256,status) VALUES('create-matches-fixture','Test dictionary','terms-v1','v1',repeat('a',64),repeat('b',64),'active') RETURNING id")
        .fetch_one(&pool).await.unwrap();
    sqlx::query("INSERT INTO dictionary.content_imports(dataset_id,input_sha256,source_locator,record_count,source_version) VALUES($1,repeat('c',64),'fixture-only',3,'content-v2')")
        .bind(dataset).execute(&pool).await.unwrap();
    for (key, term, pos, gloss) in [
        ("colour-noun-a", "colour", "noun", "visible hue"),
        ("colour-noun-b", "colour", "noun", "a flag"),
        ("color-verb", "color", "verb", "give something a hue"),
    ] {
        sqlx::query("INSERT INTO dictionary.entry_contents(dataset_id,source_key,normalized_term,pos,senses,forms,sounds,source_locator) VALUES($1,$2,$3,$4,$5,$6,$7,'fixture-only')")
            .bind(dataset).bind(key).bind(term).bind(pos)
            .bind(json!([{"glosses":[gloss]}]))
            .bind(json!([{"form":"colours","tags":["plural"]}]))
            .bind(json!([{"ipa":"/ˈkʌlə/","tags":["UK"]},{"audio":"not returned"}]))
            .execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO dictionary.region_surfaces(dataset_id,normalized_term,term,region_family,families,source_regions,evidence_types,pos,targets,is_headword) VALUES($1,'colour','colour','british_core',ARRAY['british_core'],ARRAY['UK'],ARRAY['spelling'],ARRAY['noun'],ARRAY[' ＣＯＬＯＲ ','İ','O’CLOCK　TEST'],true)")
        .bind(dataset).execute(&pool).await.unwrap();
    let (status, detected) = call(&state, Some(actor), "POST", "/detections", input).await;
    assert_eq!(status, StatusCode::OK, "{detected}");
    assert_eq!(detected["surface"], "Colour");
    assert_eq!(detected["kind"], "word");
    let matches = detected["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 3);
    assert_eq!(
        matches.iter().filter(|row| row["term"] == "colour").count(),
        2
    );
    assert_eq!(matches[0]["source_version"], "content-v2");
    assert_eq!(matches[0]["forms"][0]["text"], "colours");
    assert_eq!(matches[0]["phonetics"].as_array().unwrap().len(), 1);
    assert_ne!(matches[1]["id"], matches[2]["id"]);
    assert!(matches.iter().all(|row| row.get("is_headword").is_none()));
    let (status, reverse) = call(
        &state,
        Some(actor),
        "POST",
        "/detections",
        json!({"target_language":"en","surface":"color"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reverse["matches"], detected["matches"]);
    // Unicode lowercase and punctuation/space probes also run on CI's PostgreSQL 16.
    for (surface, kind) in [("i\u{307}", "word"), ("o'clock test", "phrase")] {
        let (status, linked) = call(
            &state,
            Some(actor),
            "POST",
            "/detections",
            json!({"target_language":"en","surface":surface}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{linked}");
        assert_eq!(linked["kind"], kind);
        assert_eq!(linked["matches"].as_array().unwrap().len(), 2);
        assert!(
            linked["matches"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["term"] == "colour")
        );
    }
    let (status, empty) = call(
        &state,
        Some(actor),
        "POST",
        "/detections",
        json!({"target_language":"en","surface":"unlisted phrase"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["kind"], "phrase");
    assert_eq!(empty["matches"], json!([]));
    for surface in ["", "12345", "中文", "bad\ninput"] {
        assert_eq!(
            call(
                &state,
                Some(actor),
                "POST",
                "/detections",
                json!({"target_language":"en","surface":surface})
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon_next.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "detection must be read-only");
    let (status, created) = call(
        &state,
        Some(actor),
        "POST",
        "/entries",
        json!({"target_language":"en","kind":"word","headword":"Colour"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        created["draft"],
        json!({"schema_version":1,"headword":"Colour","forms":[],"senses":[]})
    );
}

async fn protected_old_entry(pool: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entries(id,language,kind,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,'en','word',$2,$3,$3)")
        .bind(id).bind(json!({"headword":"centre","fixture":"protected old identity"})).bind(owner).execute(pool).await.unwrap();
    id
}
async fn old_record(pool: &PgPool, id: Uuid) -> Value {
    sqlx::query_scalar("SELECT to_jsonb(e) FROM lexicon.entries e WHERE id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn persisted_draft_and_four_independent_views(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let content = draft();
    let (status, saved) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["revision"], 2);
    // A separately built state and request must read committed storage, not a response cache.
    let reopened = AppState::for_test(pool.clone());
    let (status, fetched) = call(&reopened, Some(actor), "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched, saved);
    assert_eq!(fetched["draft"], content);
    for target in ["en-GB", "en-US"] {
        for medium in ["zh-Hans", "zh-Hant"] {
            let (status, view) = call(
                &reopened,
                Some(actor),
                "GET",
                &format!("{path}/view?target_locale={target}&medium_locale={medium}"),
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{view}");
            assert_eq!(view["revision"], 2);
            assert_eq!(view["forms"][0]["locale"], target);
            assert_eq!(view["senses"][0]["id"], content["senses"][0]["id"]);
            assert_eq!(view["senses"][0]["target_definitions"][0]["locale"], target);
            assert_eq!(view["senses"][0]["medium_definitions"][0]["locale"], medium);
            assert_eq!(view["senses"][0]["equivalents"][0]["locale"], medium);
        }
    }
    let id: Uuid = serde_json::from_value(entry["id"].clone()).unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon_next.definitions d JOIN lexicon_next.nodes n ON n.id=d.id WHERE n.entry_id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(rows, 4);
    let history: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon_next.draft_revisions WHERE entry_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(history, 2);
    assert_eq!(
        call(&reopened, Some(actor), "GET", &path, Value::Null)
            .await
            .1,
        saved
    );
}

#[sqlx::test]
async fn missing_locale_never_fabricates_and_unavailable_capabilities_reject(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool);
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let mut content = draft();
    content["forms"].as_array_mut().unwrap().truncate(1);
    content["senses"][0]["definitions"]
        .as_array_mut()
        .unwrap()
        .retain(|d| matches!(d["locale"].as_str(), Some("en-GB" | "zh-Hans")));
    content["senses"][0]["equivalents"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    assert_eq!(
        call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":content})
        )
        .await
        .0,
        StatusCode::OK
    );
    let query = format!("{path}/view?target_locale=en-US&medium_locale=zh-Hant");
    let (_, missing) = call(&state, Some(actor), "GET", &query, Value::Null).await;
    assert_eq!(missing["forms"], json!([]));
    assert_eq!(missing["senses"][0]["target_definitions"], json!([]));
    assert_eq!(missing["senses"][0]["missing_medium_definitions"], true);
    assert_eq!(missing["senses"][0]["missing_equivalents"], true);
    content["forms"][0]["locale"] = json!("en");
    content["senses"][0]["definitions"][0]["locale"] = json!("en");
    assert_eq!(
        call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":content})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, fallback) = call(&state, Some(actor), "GET", &query, Value::Null).await;
    assert_eq!(fallback["forms"][0]["locale"], "en");
    assert_eq!(
        fallback["senses"][0]["target_definitions"][0]["locale"],
        "en"
    );
    assert_eq!(fallback["senses"][0]["medium_definitions"], json!([]));
    for language in ["ja", "fr", "es", "zh"] {
        let (status, error) = call(
            &state,
            Some(actor),
            "POST",
            "/entries",
            json!({"target_language":language,"kind":"word","headword":"test"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error["code"], "capability_unavailable");
    }
    for (target, medium) in [("en-GB", "en"), ("en-AU", "zh-Hans"), ("en-US", "zh")] {
        let (status, error) = call(
            &state,
            Some(actor),
            "GET",
            &format!("{path}/view?target_locale={target}&medium_locale={medium}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error["code"], "capability_unavailable");
    }
    content["forms"][0]["locale"] = json!("fr");
    assert_eq!(
        call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":content})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (_, read) = call(&state, Some(actor), "GET", &path, Value::Null).await;
    assert_eq!(read["revision"], 3);
}

#[sqlx::test]
async fn concurrent_saves_and_permanent_node_identity_are_atomic(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let old_id = protected_old_entry(&pool, actor).await;
    let protected = old_record(&pool, old_id).await;
    assert_eq!(
        call(
            &state,
            Some(actor),
            "GET",
            &format!("/entries/{old_id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let first = create(&state, actor).await;
    let second = create(&state, actor).await;
    assert_ne!(first["id"], second["id"]); // Same spelling does not merge.
    let path = format!("/entries/{}", first["id"].as_str().unwrap());
    let other_path = format!("/entries/{}", second["id"].as_str().unwrap());
    let content = draft();
    let command = json!({"expected_revision":1,"draft":content});
    let (a, b) = tokio::join!(
        call(&state, Some(actor), "PUT", &path, command.clone()),
        call(&state, Some(actor), "PUT", &path, command)
    );
    let mut statuses = [a.0.as_u16(), b.0.as_u16()];
    statuses.sort();
    assert_eq!(statuses, [200, 409]);
    assert_eq!(
        if a.0 == StatusCode::CONFLICT {
            &a.1["code"]
        } else {
            &b.1["code"]
        },
        "revision_conflict"
    );
    // Remove all current nodes, then attempt to steal their identities for another entry.
    let empty = json!({"schema_version":1,"headword":"centre","forms":[],"senses":[]});
    assert_eq!(
        call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":empty})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, error) = call(
        &state,
        Some(actor),
        "PUT",
        &other_path,
        json!({"expected_revision":1,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["code"], "stable_node_id_changed");
    assert_eq!(
        call(&state, Some(actor), "GET", &other_path, Value::Null)
            .await
            .1,
        second
    );
    // Original IDs can be restored in their original scope.
    assert_eq!(
        call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":content})
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut moved = content.clone();
    moved["senses"][0]["id"] = json!(Uuid::now_v7());
    assert_eq!(
        call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":4,"draft":moved})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, read) = call(&state, Some(actor), "GET", &path, Value::Null).await;
    assert_eq!(read["draft"], content);
    assert_eq!(read["revision"], 4);
    let old_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.entries WHERE id=$1 OR id=$2")
            .bind(Uuid::parse_str(first["id"].as_str().unwrap()).unwrap())
            .bind(Uuid::parse_str(second["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(old_count, 0);
    assert_eq!(old_record(&pool, old_id).await, protected);
}

#[sqlx::test]
async fn current_permissions_and_strict_wire_boundaries(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let outsider = admin(&pool, EDIT).await;
    let reader = admin(&pool, &["words.access"]).await;
    let denied = admin(&pool, &[]).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    assert_eq!(
        call(&state, None, "GET", &path, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&state, Some(denied), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&state, Some(reader), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    for user in [reader, outsider] {
        assert_eq!(
            call(
                &state,
                Some(user),
                "PUT",
                &path,
                json!({"expected_revision":1,"draft":draft()})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    // Old V3 payloads and attempts to switch immutable target language fail closed.
    for body in [
        json!({"schema_version":3,"forms":{},"meanings":{}}),
        json!({"expected_revision":1,"draft":draft(),"target_language":"ja"}),
    ] {
        assert_eq!(
            call(&state, Some(actor), "PUT", &path, body).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let mut duplicate = draft();
    duplicate["senses"][0]["definitions"][0]["id"] = duplicate["forms"][0]["id"].clone();
    assert_eq!(
        call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":duplicate})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Revoke after token creation; request-time and transaction-time checks use current grants.
    sqlx::query(
        "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='words.edit'",
    )
    .bind(actor)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":draft()})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.edit_others',$1)").bind(outsider).execute(&pool).await.unwrap();
    assert_eq!(
        call(
            &state,
            Some(outsider),
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":draft()})
        )
        .await
        .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE admins SET status='disabled' WHERE id=$1")
        .bind(outsider)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, Some(outsider), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn migration_down_up_leaves_old_schema_intact(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let old_id = protected_old_entry(&pool, actor).await;
    let protected = old_record(&pool, old_id).await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261111090000_lexicon_next.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let gone: Option<String> = sqlx::query_scalar("SELECT to_regnamespace('lexicon_next')::text")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(gone.is_none());
    sqlx::raw_sql(include_str!(
        "../migrations/20261111090000_lexicon_next.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(old_record(&pool, old_id).await, protected);
}

#[sqlx::test]
async fn list_is_paginated_filtered_current_and_separate_from_legacy(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let old = protected_old_entry(&pool, actor).await;
    let old_before = old_record(&pool, old).await;
    let entry = create(&state, actor).await;
    let id = entry["id"].as_str().unwrap();
    let (status, _) = call(
        &state,
        Some(actor),
        "PUT",
        &format!("/entries/{id}"),
        json!({"expected_revision":1,"draft":draft()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, phrase) = call(
        &state,
        Some(actor),
        "POST",
        "/entries",
        json!({"target_language":"en","kind":"phrase","headword":"centre stage"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, first) = call(
        &state,
        Some(actor),
        "GET",
        "/entries?page_size=1",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["pagination"]["total"], 2);
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    let (_, second) = call(
        &state,
        Some(actor),
        "GET",
        "/entries?page_size=1&page=2",
        Value::Null,
    )
    .await;
    assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
    let (_, filtered) = call(
        &state,
        Some(actor),
        "GET",
        "/entries?headword=CENTRE&kind=word",
        Value::Null,
    )
    .await;
    assert_eq!(filtered["pagination"]["total"], 1);
    assert_eq!(filtered["items"][0]["id"], entry["id"]);
    assert_eq!(filtered["items"][0]["revision"], 2);
    assert_eq!(filtered["items"][0]["part_of_speech"], json!(["noun"]));
    assert_eq!(filtered["items"][0]["gloss"], "某物的中心位置");
    assert!(filtered["items"][0].get("draft").is_none());
    let (_, literal) = call(
        &state,
        Some(actor),
        "GET",
        "/entries?headword=%25",
        Value::Null,
    )
    .await;
    assert_eq!(literal["pagination"]["total"], 0);
    let (_, beyond) = call(&state, Some(actor), "GET", "/entries?page=100", Value::Null).await;
    assert_eq!(beyond["pagination"]["total"], 2);
    assert_eq!(beyond["items"], json!([]));
    for path in [
        "/entries?page=0",
        "/entries?page_size=101",
        "/entries?kind=other",
    ] {
        let (status, _) = call(&state, Some(actor), "GET", path, Value::Null).await;
        assert!(status.is_client_error());
    }
    assert_eq!(
        call(&state, None, "GET", "/entries", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    let no_access = admin(&pool, &[]).await;
    assert_eq!(
        call(&state, Some(no_access), "GET", "/entries", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(old_record(&pool, old).await, old_before);
    assert_ne!(phrase["id"], entry["id"]);
}

#[sqlx::test]
async fn workspace_filters_stats_and_language_drafts_stay_separate(pool: PgPool) {
    let owner = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let protected = protected_old_entry(&pool, owner).await;
    let before = old_record(&pool, protected).await;
    let first = create(&state, owner).await;
    let path = format!("/entries/{}", first["id"].as_str().unwrap());
    let content = draft();
    assert_eq!(
        call(
            &state,
            Some(owner),
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":content})
        )
        .await
        .0,
        StatusCode::OK
    );
    let other = admin(&pool, EDIT).await;
    call(
        &state,
        Some(other),
        "POST",
        "/entries",
        json!({"target_language":"en","kind":"phrase","headword":"100% centre"}),
    )
    .await;
    let (status, stats) = call(&state, Some(owner), "GET", "/workspace/stats", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_eq!(stats["total"], 2);
    assert_eq!(stats["today"], 2);
    assert_eq!(stats["month"], 2);
    let (status,list)=call(&state,Some(owner),"GET",&format!("/workspace/entries?headword=center&gloss=%E4%B8%AD%E5%BF%83&pos=noun&kind=word&created_by={owner}&page_size=10"),Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["pagination"]["total"], 1);
    assert_eq!(list["items"][0]["id"], first["id"]);
    assert_eq!(list["items"][0]["forms"].as_array().unwrap().len(), 2);
    assert_eq!(list["items"][0]["revision"], 2);
    assert_eq!(list["items"][0]["status"], "draft");
    let (_, traditional) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/entries?target_language=en&medium_locale=zh-Hant",
        Value::Null,
    )
    .await;
    // Historical Traditional text does not change this draft's Simplified ownership.
    assert_eq!(traditional["pagination"]["total"], 0);
    assert_eq!(traditional["items"], json!([]));
    let (_, target_only_text) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/entries?medium_locale=zh-Hant&gloss=middle",
        Value::Null,
    )
    .await;
    assert_eq!(target_only_text["pagination"]["total"], 0);
    for (query, total) in [
        ("headword=100%25", 1),
        ("level=A1", 0),
        ("status=published", 0),
        (
            "created_from=2000-01-01T00:00:00Z&created_to=2001-01-01T00:00:00Z",
            0,
        ),
    ] {
        let (status, list) = call(
            &state,
            Some(owner),
            "GET",
            &format!("/workspace/entries?{query}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{list}");
        assert_eq!(list["pagination"]["total"], total);
    }
    for query in [
        "page=0",
        "page_size=101",
        "status=anything",
        "level=impossible",
        "pos=madeup",
        "created_from=2026-12-01T00:00:00Z&created_to=2026-01-01T00:00:00Z",
    ] {
        assert_eq!(
            call(
                &state,
                Some(owner),
                "GET",
                &format!("/workspace/entries?{query}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let (_, creators) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/creators",
        Value::Null,
    )
    .await;
    assert_eq!(creators.as_array().unwrap().len(), 2);
    let (_, reopened) = call(&state, Some(owner), "GET", &path, Value::Null).await;
    assert_eq!(reopened["draft"], content);
    assert!(reopened.get("status").is_none());
    assert_eq!(old_record(&pool, protected).await, before);
}

#[sqlx::test]
async fn language_scope_matches_list_search_trash_stats_and_creators(pool: PgPool) {
    let owner = admin(&pool, EDIT).await;
    let foreign_owner = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let mut entries = Vec::new();
    for (target, locale, text, equivalent, archived) in [
        ("en", Some("zh-Hans"), "simplified", false, false),
        ("en", Some("zh-Hant"), "traditional", false, false),
        ("fr", Some("zh-Hans"), "simplified", false, false),
        ("en", Some("zh-Hans"), "equivalent", true, false),
        ("en", None, "", false, false),
        ("en", Some("zh-Hans"), "simplified", false, true),
    ] {
        let actor = if target == "fr" { foreign_owner } else { owner };
        let entry = create(&state, actor).await;
        let id = entry["id"].as_str().unwrap();
        if let Some(locale) = locale {
            let definition =
                json!({"id":Uuid::now_v7(),"locale":locale,"text":text,"style":"definition"});
            let equivalent_content = json!({"id":Uuid::now_v7(),"locale":locale,"text":text});
            let (status, saved) = call(
                &state,
                Some(actor),
                "PUT",
                &format!("/entries/{id}"),
                json!({
                    "expected_revision":1,
                    "draft":{"schema_version":1,"headword":"centre","forms":[],"senses":[{
                        "id":Uuid::now_v7(),"part_of_speech":"noun",
                        "definitions":if equivalent {json!([])} else {json!([definition])},
                        "equivalents":if equivalent {json!([equivalent_content])} else {json!([])}
                    }]}
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{saved}");
        }
        // Simulate historical content in a language not currently open for creation.
        sqlx::query("UPDATE lexicon_next.entries SET target_language=$2,status=$3,medium_locale=$4 WHERE id=$1")
            .bind(id.parse::<Uuid>().unwrap())
            .bind(target)
            .bind(if archived { "archived" } else { "draft" })
            .bind(locale.unwrap_or("zh-Hans"))
            .execute(&pool)
            .await
            .unwrap();
        entries.push(entry);
    }
    for (query, total) in [
        ("", 5),
        ("target_language=en", 4),
        ("target_language=fr&medium_locale=zh-Hans", 1),
        ("target_language=en&medium_locale=zh-Hans", 3),
        ("target_language=en&medium_locale=zh-Hant", 1),
        (
            "target_language=en&medium_locale=zh-Hans&gloss=traditional",
            0,
        ),
        (
            "target_language=en&medium_locale=zh-Hant&gloss=simplified",
            0,
        ),
        (
            "target_language=en&medium_locale=zh-Hans&gloss=equivalent",
            1,
        ),
        (
            "target_language=en&medium_locale=zh-Hans&status=archived",
            1,
        ),
        (
            "target_language=en&medium_locale=zh-Hant&status=archived",
            0,
        ),
    ] {
        let (status, list) = call(
            &state,
            Some(owner),
            "GET",
            &format!("/workspace/entries?{query}&page_size=1"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{query}: {list}");
        assert_eq!(list["pagination"]["total"], total, "{query}: {list}");
        assert_eq!(
            list["items"].as_array().unwrap().len(),
            usize::from(total > 0)
        );
        let (status, stats) = call(
            &state,
            Some(owner),
            "GET",
            &format!("/workspace/stats?{query}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{query}: {stats}");
        assert_eq!(
            stats,
            json!({"total":total,"today":total,"month":total}),
            "{query}"
        );
    }
    let (_, list) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/entries?target_language=en&medium_locale=zh-Hans&page_size=1&page=3",
        Value::Null,
    )
    .await;
    assert_eq!(list["pagination"]["total"], 3);
    assert_eq!(list["pagination"]["total_pages"], 3);
    assert_eq!(list["items"][0]["gloss"], "simplified");
    let (_, equivalents) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/entries?target_language=en&medium_locale=zh-Hans&gloss=equivalent",
        Value::Null,
    )
    .await;
    assert_eq!(equivalents["items"][0]["gloss"], "equivalent");
    let (_, basic) = call(
        &state,
        Some(owner),
        "GET",
        "/entries?target_language=en&medium_locale=zh-Hans",
        Value::Null,
    )
    .await;
    assert_eq!(basic["pagination"]["total"], 3);
    let (_, creators) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/creators?target_language=fr&medium_locale=zh-Hans",
        Value::Null,
    )
    .await;
    assert_eq!(
        creators,
        json!([{"id":foreign_owner,"display_name":"本批隔离测试"}])
    );
    for query in [
        "target_language=EN",
        "target_language=en-GB",
        "target_language=",
        "medium_locale=en-GB",
        "medium_locale=",
        "medium_locale=unknown",
    ] {
        for path in [
            "/workspace/entries",
            "/workspace/stats",
            "/workspace/creators",
            "/entries",
        ] {
            let (status, problem) = call(
                &state,
                Some(owner),
                "GET",
                &format!("{path}?{query}"),
                Value::Null,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{path}?{query}: {problem}"
            );
        }
    }
}

fn list_command(action: &str, targets: &[(&Value, i64)]) -> Value {
    json!({"request_id":Uuid::now_v7(),"action":action,"items":targets.iter().map(|(entry,lifecycle)|json!({"entry_id":entry["id"],"base_revision":entry["revision"],"base_lifecycle_revision":lifecycle})).collect::<Vec<_>>()})
}
#[sqlx::test]
async fn workspace_lifecycle_permissions_atomicity_and_retries(pool: PgPool) {
    let owner = admin(
        &pool,
        &[
            "words.access",
            "words.create",
            "words.edit",
            "words.archive",
            "words.restore",
        ],
    )
    .await;
    let outsider = admin(&pool, &["words.access", "words.archive", "words.restore"]).await;
    let state = AppState::for_test(pool.clone());
    let first = create(&state, owner).await;
    let second = create(&state, owner).await;
    let (status, second) = call(
        &state,
        Some(owner),
        "PUT",
        &format!("/entries/{}", second["id"].as_str().unwrap()),
        json!({"expected_revision":1,"draft":draft()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let first_id = first["id"].as_str().unwrap();
    let archive = list_command("archive", &[(&first, 1), (&second, 1)]);
    assert_eq!(
        call(
            &state,
            Some(outsider),
            "POST",
            "/workspace/commands",
            archive.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut stale = archive.clone();
    stale["items"][1]["base_revision"] = json!(999);
    assert_eq!(
        call(&state, Some(owner), "POST", "/workspace/commands", stale)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (_, normal) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/entries",
        Value::Null,
    )
    .await;
    assert_eq!(normal["pagination"]["total"], 2);
    let (status, receipt) = call(
        &state,
        Some(owner),
        "POST",
        "/workspace/commands",
        archive.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(
        call(
            &state,
            Some(owner),
            "POST",
            "/workspace/commands",
            archive.clone()
        )
        .await,
        (status, receipt)
    );
    let (_, normal) = call(&state, Some(owner), "GET", "/entries", Value::Null).await;
    assert_eq!(normal["pagination"]["total"], 0);
    let (_, trash) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/entries?status=archived",
        Value::Null,
    )
    .await;
    assert_eq!(trash["pagination"]["total"], 2);
    assert_eq!(trash["items"][0]["lifecycle_revision"], 2);
    assert_eq!(
        call(
            &state,
            Some(owner),
            "PUT",
            &format!("/entries/{first_id}"),
            json!({"expected_revision":1,"draft":draft()})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let restore = list_command("restore", &[(&first, 2)]);
    assert_eq!(
        call(&state, Some(owner), "POST", "/workspace/commands", restore)
            .await
            .0,
        StatusCode::OK
    );
    // A delayed archive request must not overwrite the intervening restore.
    assert_eq!(
        call(&state, Some(owner), "POST", "/workspace/commands", archive)
            .await
            .0,
        StatusCode::OK
    );
    let (_, read) = call(
        &state,
        Some(owner),
        "GET",
        &format!("/workspace/entries/{first_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(read["status"], "draft");
    assert_eq!(read["lifecycle_revision"], 3);
    let delete = list_command("delete", &[(&second, 2)]);
    assert_eq!(
        call(
            &state,
            Some(owner),
            "POST",
            "/workspace/commands",
            delete.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE admins SET role='super_admin' WHERE id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let deleted = call(
        &state,
        Some(owner),
        "POST",
        "/workspace/commands",
        delete.clone(),
    )
    .await;
    assert_eq!(deleted.0, StatusCode::OK, "{}", deleted.1);
    assert_eq!(
        call(
            &state,
            Some(owner),
            "POST",
            "/workspace/commands",
            delete.clone()
        )
        .await,
        deleted
    );
    assert_eq!(
        call(
            &state,
            Some(owner),
            "GET",
            &format!("/entries/{}", second["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    let removed: Uuid = serde_json::from_value(second["id"].clone()).unwrap();
    for sql in [
        "SELECT count(*) FROM lexicon_next.nodes WHERE entry_id=$1",
        "SELECT count(*) FROM lexicon_next.draft_revisions WHERE entry_id=$1",
    ] {
        let remaining: i64 = sqlx::query_scalar(sql)
            .bind(removed)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
    }
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, Some(owner), "POST", "/workspace/commands", delete)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn workspace_annotations_preserve_content_and_reject_races(pool: PgPool) {
    let owner = admin(&pool, EDIT).await;
    let outsider = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let first = create(&state, owner).await;
    let second = create(&state, owner).await;
    let path = format!(
        "/workspace/entries/{}/annotation",
        first["id"].as_str().unwrap()
    );
    let input = json!({"annotation":"1","base_annotation_revision":1});
    assert_eq!(
        call(&state, Some(outsider), "PATCH", &path, input.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, saved) = call(&state, Some(owner), "PATCH", &path, input.clone()).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["annotation_revision"], 2);
    assert_eq!(
        call(&state, Some(owner), "PATCH", &path, input.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    let path2 = format!(
        "/workspace/entries/{}/annotation",
        second["id"].as_str().unwrap()
    );
    assert_eq!(
        call(&state, Some(owner), "PATCH", &path2, input).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &state,
            Some(owner),
            "PATCH",
            &path2,
            json!({"annotation":null,"base_annotation_revision":1})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &state,
            Some(owner),
            "PATCH",
            &path2,
            json!({"annotation":"2","base_annotation_revision":1})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, read) = call(
        &state,
        Some(owner),
        "GET",
        &format!("/entries/{}", first["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(read, first);
    let (_, list) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/entries",
        Value::Null,
    )
    .await;
    assert!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["annotation_visible"] == true)
    );

    for permission in ["words.archive", "words.restore"] {
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,$2,$1)").bind(owner).bind(permission).execute(&pool).await.unwrap();
    }
    let archived = call(
        &state,
        Some(owner),
        "POST",
        "/workspace/commands",
        list_command("archive", &[(&first, 1)]),
    )
    .await;
    assert_eq!(archived.0, StatusCode::OK);
    let (_, active) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/entries",
        Value::Null,
    )
    .await;
    assert_eq!(active["items"].as_array().unwrap().len(), 1);
    assert_eq!(active["items"][0]["annotation_visible"], false);
    let (_, trash) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/entries?status=archived",
        Value::Null,
    )
    .await;
    assert_eq!(trash["items"][0]["annotation_visible"], false);
    assert_eq!(
        call(
            &state,
            Some(owner),
            "PATCH",
            &path2,
            json!({"annotation":null,"base_annotation_revision":2})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &state,
            Some(owner),
            "POST",
            "/workspace/commands",
            list_command("restore", &[(&first, 2)])
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, restored) = call(
        &state,
        Some(owner),
        "GET",
        "/workspace/entries",
        Value::Null,
    )
    .await;
    assert!(
        restored["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["annotation_visible"] == true)
    );
    assert_eq!(
        call(
            &state,
            Some(owner),
            "PATCH",
            &path2,
            json!({"annotation":"1","base_annotation_revision":3})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // The down migration refuses to discard saved annotation/lifecycle state.
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261111120000_lexicon_next_list.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
}

#[sqlx::test]
async fn workspace_migration_round_trip_preserves_language_content(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let id: Uuid = serde_json::from_value(entry["id"].clone()).unwrap();
    let original_created: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "SELECT min(created_at) FROM lexicon_next.draft_revisions WHERE entry_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261111120000_lexicon_next_list.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261111120000_lexicon_next_list.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let restored_created: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT created_at FROM lexicon_next.entries WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(original_created, restored_created);
    let (_, reopened) = call(
        &state,
        Some(actor),
        "GET",
        &format!("/entries/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(entry, reopened);
    let command = list_command("archive", &[(&entry, 1)]);
    let (a, b) = tokio::join!(
        call(
            &state,
            Some(actor),
            "POST",
            "/workspace/commands",
            command.clone()
        ),
        call(&state, Some(actor), "POST", "/workspace/commands", command)
    );
    // Missing permission is checked before any cached or concurrent command can succeed.
    assert_eq!(a.0, StatusCode::FORBIDDEN);
    assert_eq!(b.0, StatusCode::FORBIDDEN);
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.archive',$1)").bind(actor).execute(&pool).await.unwrap();
    let command = list_command("archive", &[(&entry, 1)]);
    let (a, b) = tokio::join!(
        call(
            &state,
            Some(actor),
            "POST",
            "/workspace/commands",
            command.clone()
        ),
        call(&state, Some(actor), "POST", "/workspace/commands", command)
    );
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(a, b);
    let lifecycle: i64 =
        sqlx::query_scalar("SELECT lifecycle_revision FROM lexicon_next.entries WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(lifecycle, 2);
}

#[sqlx::test]
async fn multiline_definitions_persist_and_single_line_fields_stay_strict(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let mut content = draft();
    content["senses"][0]["definitions"][0]["text"] = json!("first line\nsecond line");
    content["senses"][0]["definitions"][3]["text"] = json!("首行\r\n次行");
    let (status, saved) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let reopened = AppState::for_test(pool);
    let (status, fetched) = call(&reopened, Some(actor), "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["draft"], content);
    let (status, view) = call(
        &reopened,
        Some(actor),
        "GET",
        &format!("{path}/view?target_locale=en-GB&medium_locale=zh-Hant"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        view["senses"][0]["target_definitions"][0]["text"],
        "first line\nsecond line"
    );
    assert_eq!(
        view["senses"][0]["medium_definitions"][0]["text"],
        "首行\r\n次行"
    );
    for (pointer, text) in [
        ("/headword", "cen\ntre".to_owned()),
        ("/forms/0/text", "cen\ntre".to_owned()),
        ("/senses/0/equivalents/0/text", "中\r\n心".to_owned()),
        ("/senses/0/definitions/0/text", "first\tsecond".to_owned()),
        ("/senses/0/definitions/0/text", "first\0second".to_owned()),
        (
            "/senses/0/definitions/0/text",
            "first\u{85}second".to_owned(),
        ),
        ("/senses/0/definitions/0/text", "\r\n".to_owned()),
        (
            "/senses/0/definitions/0/text",
            format!("{}\n", "a".repeat(5000)),
        ),
    ] {
        let mut invalid = content.clone();
        *invalid.pointer_mut(pointer).unwrap() = json!(text);
        let (status, error) = call(
            &reopened,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":invalid}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{pointer}: {error}"
        );
    }
    assert_eq!(
        call(&reopened, Some(actor), "GET", &path, Value::Null)
            .await
            .1,
        fetched
    );
}

#[sqlx::test]
async fn homonym_annotations_use_only_initial_headwords(pool: PgPool) {
    let actor = admin(
        &pool,
        &[
            "words.access",
            "words.create",
            "words.edit",
            "words.archive",
        ],
    )
    .await;
    let state = AppState::for_test(pool);
    let form_only = create(&state, actor).await;
    let mut form_only_draft = draft();
    form_only_draft["headword"] = json!("flip");
    form_only_draft["forms"] = json!([{
        "id":Uuid::now_v7(),
        "locale":"en-US",
        "text":"flipper"
    }]);
    let (status, first) = call(
        &state,
        Some(actor),
        "PUT",
        &format!("/entries/{}", form_only["id"].as_str().unwrap()),
        json!({"expected_revision":1,"draft":form_only_draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, only_form_matches) = call_annotation_rule(
        &state,
        actor,
        "POST",
        "/entry-matches",
        json!({
            "target_language":"en",
            "surface":"flipper"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{only_form_matches}");
    assert_eq!(only_form_matches["annotation_required"], false);
    let first = call(
        &state,
        Some(actor),
        "POST",
        "/entries",
        json!({"target_language":"en","kind":"word","headword":"flipper"}),
    )
    .await
    .1;
    let (status, second) = call(
        &state,
        Some(actor),
        "POST",
        "/entries",
        json!({"target_language":"en","kind":"word","headword":"flipper"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, list) = call(
        &state,
        Some(actor),
        "GET",
        "/workspace/entries",
        Value::Null,
    )
    .await;
    for (entry, visible) in [(&form_only, false), (&first, true), (&second, true)] {
        let row = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == entry["id"])
            .unwrap();
        assert_eq!(row["annotation_visible"], visible, "{row}");
        let (_, read) = call(
            &state,
            Some(actor),
            "GET",
            &format!("/workspace/entries/{}", entry["id"].as_str().unwrap()),
            Value::Null,
        )
        .await;
        assert_eq!(read["annotation_visible"], visible);
    }
    let (status, matches) = call_annotation_rule(
        &state,
        actor,
        "POST",
        "/entry-matches",
        json!({
            "target_language":"en",
            "surface":"flipper"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{matches}");
    assert_eq!(matches["annotation_required"], true);
    assert_eq!(matches["annotation_items"].as_array().unwrap().len(), 2);
    assert_eq!(matches["pagination"]["total"], 3);

    for (entry, annotation, expected) in [
        (&form_only, Value::Null, StatusCode::OK),
        (&first, json!("1"), StatusCode::OK),
        (&second, json!("1"), StatusCode::CONFLICT),
    ] {
        let (status, result) = call(
            &state,
            Some(actor),
            "PATCH",
            &format!(
                "/workspace/entries/{}/annotation",
                entry["id"].as_str().unwrap()
            ),
            json!({"annotation":annotation,"base_annotation_revision":1}),
        )
        .await;
        assert_eq!(status, expected, "{result}");
    }
    let (status, result) = call(
        &state,
        Some(actor),
        "PATCH",
        &format!(
            "/workspace/entries/{}/annotation",
            second["id"].as_str().unwrap()
        ),
        json!({"annotation":"2","base_annotation_revision":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");

    let (status, result) = call(
        &state,
        Some(actor),
        "POST",
        "/workspace/commands",
        list_command("archive", &[(&first, 1)]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let (_, remaining) = call(
        &state,
        Some(actor),
        "GET",
        "/workspace/entries",
        Value::Null,
    )
    .await;
    assert_eq!(remaining["pagination"]["total"], 2);
    assert!(
        remaining["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == second["id"])
            .is_some_and(|row| row["annotation_visible"] == false)
    );
}

fn form_editor() -> Value {
    let pos = Uuid::now_v7();
    let content = Uuid::now_v7();
    let form = Uuid::now_v7();
    let uk = Uuid::now_v7();
    let us = Uuid::now_v7();
    let uk_pron = Uuid::now_v7();
    let us_pron = Uuid::now_v7();
    json!({"forms_format":"shared_v1","contents":[{
        "id":content,"dialect_rules":{"spelling_mode":"distinguish","phonetic_mode":"distinguish"},
        "regional_variants":{"mode":"uk_us",
            "uk":{"id":uk,"spelling":"centre","origin":"manual","pronunciations":[{"id":uk_pron,"dict_phonetic":"sentə","actual_pron":"sentə","style":"citation"}]},
            "us":{"id":us,"spelling":"center","origin":"manual","pronunciations":[{"id":us_pron,"dict_phonetic":"sentər","actual_pron":"sentər","style":"citation"}]}
        }}],"pos":[{"pos_id":pos,"pos":"noun","forms":[{
            "id":form,"form_type":"base","content_id":content,
            "regional_bindings":{"mode":"uk_us",
                "uk":{"id":Uuid::now_v7(),"content_slot_id":uk,"is_regular":true,"component_usages":[],"pronunciations":[{"id":Uuid::now_v7(),"content_slot_id":uk_pron}]},
                "us":{"id":Uuid::now_v7(),"content_slot_id":us,"is_regular":true,"component_usages":[],"pronunciations":[{"id":Uuid::now_v7(),"content_slot_id":us_pron}]}
            }}],"form_groups":[{"id":Uuid::now_v7(),"is_regular":true,"scope":"general","members":[{"id":Uuid::now_v7(),"form_id":form}]}]}]})
}

async fn workspace_list_with_names(state: &AppState, actor: Uuid) -> Value {
    let req = Request::builder()
        .uri(format!("{ROOT}/workspace/entries?headword=center&pos=noun"))
        .header("x-tsz-next-name-variants", "v1")
        .header(
            "authorization",
            format!(
                "Bearer {}",
                state.admin_token_manager.generate(actor, "admin").unwrap()
            ),
        )
        .body(Body::empty())
        .unwrap();
    let response = tsz_rust::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[sqlx::test]
async fn workspace_names_use_saved_base_forms_and_deduplicate_shared_spellings(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool);
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let mut draft = entry["draft"].clone();
    let mut editor = form_editor();
    let mut plural = form_editor();
    plural["contents"][0]["regional_variants"]["uk"]["spelling"] = json!("centres");
    plural["contents"][0]["regional_variants"]["us"]["spelling"] = json!("centers");
    plural["pos"][0]["forms"][0]["form_type"] = json!("plural");
    editor["contents"]
        .as_array_mut()
        .unwrap()
        .push(plural["contents"][0].take());
    editor["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .push(plural["pos"][0]["forms"][0].take());
    editor["pos"][0]["form_groups"][0]["members"]
        .as_array_mut()
        .unwrap()
        .push(plural["pos"][0]["form_groups"][0]["members"][0].take());
    draft["form_editor"] = editor;
    let (status, saved) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let (_, old_list) = call(
        &state,
        Some(actor),
        "GET",
        "/workspace/entries?headword=center&pos=noun",
        Value::Null,
    )
    .await;
    let old_row = &old_list["items"][0];
    assert_eq!(old_row["id"], entry["id"]);
    assert!(old_row.get("name_variants").is_none());

    let list = workspace_list_with_names(&state, actor).await;
    assert_eq!(list["items"][0]["id"], entry["id"]);
    assert_eq!(
        list["items"][0]["name_variants"],
        json!([
            {"text":"centre","dialects":["uk"]},
            {"text":"center","dialects":["us"]}
        ])
    );

    draft["form_editor"]["contents"][0]["regional_variants"]["uk"]["spelling"] = json!("center");
    let (status, saved) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let list = workspace_list_with_names(&state, actor).await;
    assert_eq!(
        list["items"][0]["name_variants"],
        json!([{"text":"center","dialects":["uk","us"]}])
    );

    // A spelling allowed at save time can grow beyond the input limit after NFKC.
    draft["form_editor"]["contents"][0]["regional_variants"]["uk"]["spelling"] =
        json!("ﬀ".repeat(101));
    let (status, saved) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let list = workspace_list_with_names(&state, actor).await;
    assert_eq!(
        list["items"][0]["name_variants"],
        json!([
            {"text":"ff".repeat(101),"dialects":["uk"]},
            {"text":"center","dialects":["us"]}
        ])
    );
}

#[sqlx::test]
async fn v1_forms_editor_round_trips_and_projects_without_legacy_entry_data(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let mut draft = entry["draft"].clone();
    let editor = form_editor();
    draft["form_editor"] = editor.clone();
    let (status, saved) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, reopened) = call(&state, Some(actor), "GET", &path, Value::Null).await;
    assert_eq!(reopened["draft"]["form_editor"], editor);
    assert_eq!(reopened["draft"]["forms"], json!([]));
    let (_, view) = call(
        &state,
        Some(actor),
        "GET",
        &format!("{path}/view?target_locale=en-US&medium_locale=zh-Hans"),
        Value::Null,
    )
    .await;
    assert_eq!(view["forms"][0]["text"], "center");
    let (_, list) = call(
        &state,
        Some(actor),
        "GET",
        "/workspace/entries?headword=center&pos=noun",
        Value::Null,
    )
    .await;
    assert_eq!(list["items"][0]["id"], entry["id"]);
    assert_eq!(list["items"][0]["part_of_speech"], json!(["noun"]));
    assert_eq!(list["items"][0]["forms"].as_array().unwrap().len(), 2);
    // A cached old client must not silently erase the editor content.
    let mut old = draft.clone();
    old.as_object_mut().unwrap().remove("form_editor");
    let (status, _) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":old}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let other = create(&state, actor).await;
    let (status, _) = call(
        &state,
        Some(actor),
        "PUT",
        &format!("/entries/{}", other["id"].as_str().unwrap()),
        json!({"expected_revision":1,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let mut invalid = saved["draft"].clone();
    invalid["form_editor"]["pos"][0]["forms"][0]["regional_bindings"]["uk"]["content_slot_id"] =
        json!(Uuid::now_v7());
    let (status, _) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":invalid}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, unchanged) = call(&state, Some(actor), "GET", &path, Value::Null).await;
    assert_eq!(unchanged["revision"], 2);
    assert_eq!(unchanged["draft"]["form_editor"], editor);
    let mut collision = saved["draft"].clone();
    collision["senses"] = json!([{"id":editor["pos"][0]["pos_id"],"part_of_speech":null,"definitions":[],"equivalents":[]}]);
    let (status, _) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":collision}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut invalid_null = saved["draft"].clone();
    invalid_null["form_editor"] = Value::Null;
    let (status, _) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":invalid_null}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261113120000_lexicon_next_form_editor.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
    let (_, after_refused_rollback) = call(&state, Some(actor), "GET", &path, Value::Null).await;
    assert_eq!(after_refused_rollback["draft"]["form_editor"], editor);
}

#[sqlx::test]
async fn form_editor_recordings_are_canonical_owned_and_protected(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let asset = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id) VALUES($1,$2,$3,'audio/mpeg',3,'en-GB','female','fixture.mp3',$4)")
        .bind(asset).bind(format!("local-fixture/{asset}"))
        .bind(format!("local-fixture/source/{asset}")).bind(actor).execute(&pool).await.unwrap();
    let metadata: Value = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'locale',locale,'gender',gender,'content_type',content_type,'size_bytes',size_bytes,'duration_ms',duration_ms,'original_name',original_name,'created_at',created_at,'is_default',true) FROM lexicon.audio_assets WHERE id=$1")
        .bind(asset).fetch_one(&pool).await.unwrap();
    let mut editor = form_editor();
    let mut forged = metadata.clone();
    forged["original_name"] = json!("forged.mp3");
    editor["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["audio_assets"] =
        json!([forged]);
    let mut draft = entry["draft"].clone();
    draft["form_editor"] = editor;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let (status, saved) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        serde_json::from_value::<tsz_rust::lexicon::audio_assets::dto::AudioAsset>(saved["draft"]["form_editor"]["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["audio_assets"][0].clone()).unwrap(),
        serde_json::from_value::<tsz_rust::lexicon::audio_assets::dto::AudioAsset>(metadata.clone()).unwrap()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon_next.audio_asset_references WHERE asset_id=$1",
    )
    .bind(asset)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let other = create(&state, actor).await;
    let mut another = other["draft"].clone();
    let mut another_editor = form_editor();
    another_editor["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["audio_assets"] =
        json!([metadata]);
    another["form_editor"] = another_editor;
    let (status, _) = call(
        &state,
        Some(actor),
        "PUT",
        &format!("/entries/{}", other["id"].as_str().unwrap()),
        json!({"expected_revision":1,"draft":another}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut removed = saved["draft"].clone();
    removed["form_editor"]["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["audio_assets"] =
        json!([]);
    let (status, result) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":removed}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon_next.audio_asset_references WHERE asset_id=$1",
    )
    .bind(asset)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM lexicon.audio_assets WHERE id=$1)"
        )
        .bind(asset)
        .fetch_one(&pool)
        .await
        .unwrap()
    );
}

#[sqlx::test]
async fn sense_groups_round_trip_reorder_detach_and_protect_legacy_clients(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let reader = admin(&pool, &["words.access"]).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let (_, old_caps) = call(&state, Some(actor), "GET", "/capabilities", Value::Null).await;
    assert!(old_caps.get("sense_groups").is_none());
    let (_, caps) = call_with_groups(
        &state,
        Some(actor),
        "GET",
        "/capabilities",
        Value::Null,
        true,
    )
    .await;
    assert_eq!(caps["sense_groups"], true);
    let g1 = Uuid::now_v7();
    let g2 = Uuid::now_v7();
    let mut content = draft();
    content["sense_groups"] = json!([{ "id":g1,"name_zh":"位置","name_en":"position" },{ "id":g2,"name_zh":"机构","name_en":"institution" }]);
    content["senses"][0]["sense_group_id"] = json!(g1);
    let (status, saved) = call_with_groups(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["draft"], content);
    let (_, read) = call_with_groups(&state, Some(actor), "GET", &path, Value::Null, true).await;
    assert_eq!(read["draft"], content);
    let (_, legacy) = call(&state, Some(actor), "GET", &path, Value::Null).await;
    assert!(legacy["draft"].get("sense_groups").is_none());
    assert!(legacy["draft"]["senses"][0].get("sense_group_id").is_none());
    let (status, _) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":legacy["draft"]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call_with_groups(
        &state,
        Some(reader),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":content}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    content["sense_groups"].as_array_mut().unwrap().reverse();
    content["senses"][0]["sense_group_id"] = json!(g2);
    let (status, saved) = call_with_groups(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":content}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (status, preview) = call_with_groups(
        &state,
        Some(actor),
        "GET",
        &format!("{path}/view?target_locale=en-US&medium_locale=zh-Hant"),
        Value::Null,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["sense_groups"], content["sense_groups"]);
    assert_eq!(preview["senses"][0]["sense_group_id"], json!(g2));
    let (status, legacy_view) = call(
        &state,
        Some(actor),
        "GET",
        &format!("{path}/view?target_locale=en-US&medium_locale=zh-Hant"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(legacy_view.get("sense_groups").is_none());
    assert!(legacy_view["senses"][0].get("sense_group_id").is_none());

    content["sense_groups"].as_array_mut().unwrap().remove(0);
    content["senses"][0]
        .as_object_mut()
        .unwrap()
        .remove("sense_group_id");
    let (status, saved) = call_with_groups(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":content}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["draft"]["senses"][0]["definitions"],
        content["senses"][0]["definitions"]
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon_next.nodes WHERE entry_id=$1 AND kind='sense_group'",
    )
    .bind(Uuid::parse_str(entry["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 2);
    let other = create(&state, actor).await;
    let mut other_content = draft();
    other_content["sense_groups"] = json!([{ "id":g2,"name_zh":"归属测试","name_en":"ownership" }]);
    other_content["senses"][0]["sense_group_id"] = json!(g2);
    let (status, _) = call_with_groups(
        &state,
        Some(actor),
        "PUT",
        &format!("/entries/{}", other["id"].as_str().unwrap()),
        json!({"expected_revision":1,"draft":other_content}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, read) = call_with_groups(&state, Some(actor), "GET", &path, Value::Null, true).await;
    assert_eq!(read["revision"], 4);
    assert_eq!(read["draft"], content);
}

#[sqlx::test]
async fn sense_groups_migration_reverses_legacy_data_and_refuses_extended_snapshots(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let down = include_str!("../migrations/20261113140000_lexicon_next_sense_groups.down.sql");
    let up = include_str!("../migrations/20261113140000_lexicon_next_sense_groups.up.sql");
    sqlx::raw_sql(down).execute(&pool).await.unwrap();
    let (_, legacy) = call(
        &state,
        Some(actor),
        "GET",
        &format!("/entries/{}", entry["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(legacy["draft"], entry["draft"]);
    sqlx::raw_sql(up).execute(&pool).await.unwrap();
    let mut content = entry["draft"].clone();
    content["sense_groups"] = json!([{ "id":Uuid::now_v7(),"name_zh":"","name_en":"" }]);
    let (status, saved) = call_with_groups(
        &state,
        Some(actor),
        "PUT",
        &format!("/entries/{}", entry["id"].as_str().unwrap()),
        json!({"expected_revision":1,"draft":content}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(sqlx::raw_sql(down).execute(&pool).await.is_err());
    let (_, read) = call_with_groups(
        &state,
        Some(actor),
        "GET",
        &format!("/entries/{}", entry["id"].as_str().unwrap()),
        Value::Null,
        true,
    )
    .await;
    assert_eq!(read["draft"], content);
}

#[sqlx::test]
async fn medium_ownership_keeps_empty_drafts_isolated_across_workspace_and_matches(pool: PgPool) {
    let grants = &[
        "words.access",
        "words.create",
        "words.edit",
        "words.archive",
        "words.restore",
    ];
    let hans_actor = admin(&pool, grants).await;
    let hant_actor = admin(&pool, grants).await;
    let state = AppState::for_test(pool.clone());
    let mut entries = Vec::new();
    for (actor, medium) in [(hans_actor, "zh-Hans"), (hant_actor, "zh-Hant")] {
        let (status, value) = call_scoped(&state, actor, "POST", "/entries", json!({"target_language":"en", "medium_locale":medium, "kind":"word", "headword":"centre"})).await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
        assert_eq!(value["medium_locale"], medium);
        entries.push(value);
    }
    for (index, medium) in ["zh-Hans", "zh-Hant"].iter().enumerate() {
        let (_, list) = call_scoped(
            &state,
            hans_actor,
            "GET",
            &format!("/workspace/entries?target_language=en&medium_locale={medium}"),
            Value::Null,
        )
        .await;
        assert_eq!(list["pagination"]["total"], 1);
        assert_eq!(list["items"][0]["id"], entries[index]["id"]);
        assert_eq!(
            list["items"][0]["annotation_visible"], false,
            "another medium does not create a same-name conflict"
        );
        let (_, stats) = call_scoped(
            &state,
            hans_actor,
            "GET",
            &format!("/workspace/stats?medium_locale={medium}"),
            Value::Null,
        )
        .await;
        assert_eq!(stats["total"], 1);
        let (_, creators) = call_scoped(
            &state,
            hans_actor,
            "GET",
            &format!("/workspace/creators?medium_locale={medium}"),
            Value::Null,
        )
        .await;
        assert_eq!(creators.as_array().unwrap().len(), 1);
        assert_eq!(
            creators[0]["id"],
            if index == 0 {
                hans_actor.to_string()
            } else {
                hant_actor.to_string()
            }
        );
        let (_, matches) = call_scoped(
            &state,
            hans_actor,
            "POST",
            "/entry-matches",
            json!({"target_language":"en","medium_locale":medium,"surface":"centre"}),
        )
        .await;
        assert_eq!(matches["pagination"]["total"], 1);
        assert_eq!(matches["items"][0]["id"], entries[index]["id"]);
    }
    let id = entries[0]["id"].as_str().unwrap();
    let (status, saved) = call_scoped(
        &state,
        hans_actor,
        "PUT",
        &format!("/entries/{id}"),
        json!({"expected_revision":1,"draft":draft()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["medium_locale"], "zh-Hans");
    let mut blank = saved["draft"].clone();
    blank["senses"] = json!([]);
    let (status, _) = call_scoped(
        &state,
        hans_actor,
        "PUT",
        &format!("/entries/{id}"),
        json!({"expected_revision":2,"draft":blank}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, reopened) = call_scoped(
        &state,
        hans_actor,
        "GET",
        &format!("/entries/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(reopened["medium_locale"], "zh-Hans");
    assert_eq!(reopened["draft"]["senses"], json!([]));
    let (_, list) = call_scoped(
        &state,
        hans_actor,
        "GET",
        "/workspace/entries?medium_locale=zh-Hans",
        Value::Null,
    )
    .await;
    assert_eq!(list["pagination"]["total"], 1);
    assert_eq!(list["items"][0]["id"], entries[0]["id"]);
    assert_eq!(
        call_scoped(
            &state,
            hans_actor,
            "GET",
            &format!("/entries/{id}/view?target_locale=en-GB&medium_locale=zh-Hant"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let id = entries[1]["id"].as_str().unwrap();
    let (_, row) = call_scoped(
        &state,
        hant_actor,
        "GET",
        &format!("/workspace/entries/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(call_scoped(&state, hant_actor, "POST", "/workspace/commands", json!({"request_id":Uuid::now_v7(),"action":"archive","items":[{"entry_id":id,"base_revision":row["revision"],"base_lifecycle_revision":row["lifecycle_revision"]}]})).await.0, StatusCode::OK);
    for (medium, total) in [("zh-Hans", 0), ("zh-Hant", 1)] {
        let (_, trash) = call_scoped(
            &state,
            hans_actor,
            "GET",
            &format!("/workspace/entries?status=archived&medium_locale={medium}"),
            Value::Null,
        )
        .await;
        assert_eq!(trash["pagination"]["total"], total);
    }
    let (_, old) = call(
        &state,
        Some(hans_actor),
        "GET",
        &format!("/entries/{}", entries[0]["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert!(
        old.get("medium_locale").is_none(),
        "old strict clients keep the old response shape"
    );
    let (_, caps) = call_scoped(&state, hans_actor, "GET", "/capabilities", Value::Null).await;
    assert_eq!(caps["medium_ownership"], true);
    let (_, old_caps) = call(
        &state,
        Some(hans_actor),
        "GET",
        "/capabilities",
        Value::Null,
    )
    .await;
    assert!(old_caps.get("medium_ownership").is_none());
    assert_eq!(
        call_scoped(
            &state,
            hans_actor,
            "POST",
            "/entries",
            json!({"target_language":"en","medium_locale":"fr","kind":"word","headword":"test"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[sqlx::test]
async fn medium_ownership_migration_preserves_legacy_text_and_round_trips(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let mut documents = Vec::new();
    for locales in [
        vec![],
        vec!["zh-Hans"],
        vec!["zh-Hant"],
        vec!["zh-Hans", "zh-Hant"],
    ] {
        let value = create(&state, actor).await;
        let id = value["id"].as_str().unwrap();
        let mut content = draft();
        content["senses"][0]["definitions"]
            .as_array_mut()
            .unwrap()
            .retain(|row| {
                row["locale"]
                    .as_str()
                    .is_some_and(|locale| locale.starts_with("en") || locales.contains(&locale))
            });
        content["senses"][0]["equivalents"]
            .as_array_mut()
            .unwrap()
            .retain(|row| locales.contains(&row["locale"].as_str().unwrap()));
        let (status, saved) = call(
            &state,
            Some(actor),
            "PUT",
            &format!("/entries/{id}"),
            json!({"expected_revision":1,"draft":content}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        documents.push((Uuid::parse_str(id).unwrap(), saved["draft"].clone()));
    }
    sqlx::raw_sql(include_str!(
        "../migrations/20261114100000_lexicon_next_medium_ownership.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert!(!sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema='lexicon_next' AND table_name='entries' AND column_name='medium_locale')").fetch_one(&pool).await.unwrap());
    sqlx::raw_sql(include_str!(
        "../migrations/20261114100000_lexicon_next_medium_ownership.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    for ((id, original), expected) in documents
        .iter()
        .zip(["zh-Hans", "zh-Hans", "zh-Hant", "zh-Hans"])
    {
        let (medium, document): (String, Value) = sqlx::query_as("SELECT e.medium_locale,r.document FROM lexicon_next.entries e JOIN lexicon_next.draft_revisions r ON r.entry_id=e.id AND r.revision=e.revision WHERE e.id=$1").bind(id).fetch_one(&pool).await.unwrap();
        assert_eq!(medium, expected);
        assert_eq!(&document, original);
    }
}

async fn call_grammar(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    call_client(state, Some(actor), method, path, body, (true, true, true)).await
}

async fn call_grammar_links(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(format!("{ROOT}{path}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "Bearer {}",
                state.admin_token_manager.generate(actor, "admin").unwrap()
            ),
        )
        .header("x-tsz-next-forms", "v1")
        .header("x-tsz-next-grammar", "v1")
        .header("x-tsz-next-grammar-form-links", "v1")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = tsz_rust::router(state.clone()).oneshot(req).await.unwrap();
    let status = response.status();
    let data =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, data)
}

#[sqlx::test]
async fn grammar_rich_variants_bind_subcategories_and_protect_legacy_drafts(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool, EDIT).await;
    // Reversible before new-format snapshots exist; afterwards down must fail closed.
    sqlx::raw_sql(include_str!(
        "../migrations/20261114130000_lexicon_next_grammar.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261114130000_lexicon_next_grammar.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let (_, old_caps) = call(&state, Some(actor), "GET", "/capabilities", Value::Null).await;
    assert!(old_caps.get("grammar_structures").is_none());
    let (_, caps) = call_grammar(&state, actor, "GET", "/capabilities", Value::Null).await;
    assert_eq!(caps["grammar_structures"], true);
    assert_eq!(caps["publishing"], true);
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let grammar_id = Uuid::now_v7();
    let mut content = draft();
    content["grammar_structures"] = json!([{
        "id":grammar_id,"part_of_speech":"noun","sub_pos":"N-COUNT",
        "variants":[
            {"id":Uuid::now_v7(),"locale":"en-GB","content":{"version":2,"text":"a job","annotations":[{"type":"emphasis","start":2,"end":5,"level":"core"}]}},
            {"id":Uuid::now_v7(),"locale":"en-US","content":{"version":2,"text":"a role","annotations":[{"type":"italic","start":2,"end":6}]}}
        ]
    }]);
    content["senses"][0]["sub_pos"] = json!("N-COUNT");
    for definition in content["senses"][0]["definitions"].as_array_mut().unwrap() {
        definition["grammar_structure_id"] = json!(grammar_id);
    }
    let (status, saved) = call_grammar(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, reopened) = call_grammar(&state, actor, "GET", &path, Value::Null).await;
    assert_eq!(reopened["draft"], content);
    let (_, legacy) = call(&state, Some(actor), "GET", &path, Value::Null).await;
    assert!(legacy["draft"].get("grammar_structures").is_none());
    assert!(legacy["draft"]["senses"][0].get("sub_pos").is_none());
    assert!(
        legacy["draft"]["senses"][0]["definitions"][0]
            .get("grammar_structure_id")
            .is_none()
    );
    let (status, _) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":legacy["draft"]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let view_path = format!("{path}/view?target_locale=en-US&medium_locale=zh-Hans");
    let (_, view) = call_grammar(&state, actor, "GET", &view_path, Value::Null).await;
    assert_eq!(view["grammar_issues"], json!([]));
    assert_eq!(
        view["grammar_structures"][0]["variants"][0]["content"]["text"],
        "a role"
    );
    assert_eq!(
        view["grammar_structures"][0]["variants"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    content["grammar_structures"][0]["sub_pos"] = json!("N-UNCOUNT");
    let (status, pending) = call_grammar(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pending}");
    assert_eq!(
        pending["draft"]["senses"][0]["definitions"][0]["grammar_structure_id"],
        json!(grammar_id)
    );
    let (_, view) = call_grammar(&state, actor, "GET", &view_path, Value::Null).await;
    assert!(
        view["grammar_issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["field"] == "grammar_structure_id")
    );
    assert!(
        view["senses"][0]["target_definitions"][0]
            .get("grammar_structure_id")
            .is_none()
    );
    let mut wrong = content.clone();
    wrong["grammar_structures"][0]["sub_pos"] = json!("V-T");
    let (status, _) = call_grammar(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":wrong}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut wrong = content.clone();
    wrong["grammar_structures"][0]["variants"][0]["content"]["annotations"][0]["end"] = json!(60);
    let (status, _) = call_grammar(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":wrong}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut wrong = content.clone();
    wrong["grammar_structures"][0]["variants"][0]["locale"] = json!("en-US");
    wrong["grammar_structures"][0]["variants"][1]["locale"] = json!("en-GB");
    let (status, _) = call_grammar(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":wrong}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, current) = call_grammar(&state, actor, "GET", &path, Value::Null).await;
    assert_eq!(current["revision"], 3);
    let other = create(&state, actor).await;
    let other_path = format!("/entries/{}", other["id"].as_str().unwrap());
    let mut other_draft = draft();
    other_draft["form_editor"] = form_editor();
    other_draft["form_editor"]["contents"][0]["id"] = json!(grammar_id);
    other_draft["form_editor"]["pos"][0]["forms"][0]["content_id"] = json!(grammar_id);
    let (status, collision) = call_grammar(
        &state,
        actor,
        "PUT",
        &other_path,
        json!({"expected_revision":1,"draft":other_draft}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{collision}");
    let mut restored = content.clone();
    restored["grammar_structures"][0]["sub_pos"] = json!("N-COUNT");
    let (status, _) = call_grammar(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":restored}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    sqlx::query(
        "UPDATE catalog.sub_parts_of_speech SET code='N-COUNT-CHANGED' WHERE code='N-COUNT'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (_, stale_view) = call_grammar(&state, actor, "GET", &view_path, Value::Null).await;
    assert_eq!(
        stale_view["grammar_structures"],
        json!([]),
        "目录失效结构不能继续作为有效投影：{stale_view}"
    );
    assert!(
        stale_view["senses"][0]["target_definitions"][0]
            .get("grammar_structure_id")
            .is_none()
    );
    for node in [
        restored["grammar_structures"][0]["id"].clone(),
        restored["senses"][0]["id"].clone(),
    ] {
        assert!(
            stale_view["grammar_issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|issue| issue["node_id"] == node && issue["field"] == "sub_pos")
        );
    }
    sqlx::query(
        "UPDATE catalog.sub_parts_of_speech SET code='N-COUNT' WHERE code='N-COUNT-CHANGED'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261114130000_lexicon_next_grammar.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
}

#[sqlx::test]
async fn grammar_links_survive_legacy_save_and_reject_invalid_ranges(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool, EDIT).await;
    let target = create(&state, actor).await;
    let target_entry = Uuid::parse_str(target["id"].as_str().unwrap()).unwrap();
    let target_pos = Uuid::now_v7();
    let target_form = Uuid::now_v7();
    let target_variant = Uuid::now_v7();
    for (id, role, parent) in [
        (target_pos, "pos", None),
        (target_form, "form", Some(target_pos)),
        (target_variant, "variant", Some(target_form)),
    ] {
        sqlx::query("INSERT INTO lexicon_next.form_editor_nodes(id,entry_id,role,parent_id) VALUES($1,$2,$3,$4)")
            .bind(id)
            .bind(target_entry)
            .bind(role)
            .bind(parent)
            .execute(&pool)
            .await
            .unwrap();
    }
    let source = create(&state, actor).await;
    let path = format!("/entries/{}", source["id"].as_str().unwrap());
    let grammar_id = Uuid::now_v7();
    let gb_variant = Uuid::now_v7();
    let us_variant = Uuid::now_v7();
    let link_id = Uuid::now_v7();
    let mut content = draft();
    content["grammar_structures"] = json!([{
        "id":grammar_id,
        "part_of_speech":"noun",
        "variants":[
            {"id":gb_variant,"locale":"en-GB","content":{"version":2,"text":"target","annotations":[]},"form_links":[{
                "id":link_id,
                "source_segments":[{"start":0,"end":6,"surface":"target"}],
                "target_source":"lexicon_next",
                "target_word_id":target_entry,
                "target_pos_id":target_pos,
                "target_form_id":target_form,
                "target_variant_id":target_variant,
                "target_dialect":"common"
            }]},
            {"id":us_variant,"locale":"en-US","content":{"version":2,"text":"target","annotations":[]}}
        ]
    }]);
    let (status, saved) = call_grammar_links(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["draft"]["grammar_structures"][0]["variants"][0]["form_links"][0]["target_source"],
        "lexicon_next"
    );
    let (_, reopened) = call_grammar_links(&state, actor, "GET", &path, Value::Null).await;
    assert_eq!(reopened["draft"], saved["draft"]);

    let mut invalid = content.clone();
    invalid["grammar_structures"][0]["variants"][0]["form_links"][0]["source_segments"][0]["start"] =
        json!(1);
    assert_eq!(
        call_grammar_links(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":invalid}),
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let mut legacy = saved["draft"].clone();
    for variant in legacy["grammar_structures"][0]["variants"]
        .as_array_mut()
        .unwrap()
    {
        variant.as_object_mut().unwrap().remove("form_links");
    }
    let (status, legacy_saved) = call_grammar(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":legacy}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{legacy_saved}");
    let (_, retained) = call_grammar_links(&state, actor, "GET", &path, Value::Null).await;
    assert_eq!(
        retained["draft"]["grammar_structures"][0]["variants"][0]["form_links"][0]["id"],
        link_id.to_string()
    );
}

#[sqlx::test]
async fn grammar_links_reject_phrase_targets(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool, EDIT).await;
    let target = create(&state, actor).await;
    let target_entry = Uuid::parse_str(target["id"].as_str().unwrap()).unwrap();
    sqlx::query("UPDATE lexicon_next.entries SET kind='phrase' WHERE id=$1")
        .bind(target_entry)
        .execute(&pool)
        .await
        .unwrap();
    let target_pos = Uuid::now_v7();
    let target_form = Uuid::now_v7();
    let target_variant = Uuid::now_v7();
    for (id, role, parent) in [
        (target_pos, "pos", None),
        (target_form, "form", Some(target_pos)),
        (target_variant, "variant", Some(target_form)),
    ] {
        sqlx::query("INSERT INTO lexicon_next.form_editor_nodes(id,entry_id,role,parent_id) VALUES($1,$2,$3,$4)")
            .bind(id)
            .bind(target_entry)
            .bind(role)
            .bind(parent)
            .execute(&pool)
            .await
            .unwrap();
    }
    let source = create(&state, actor).await;
    let path = format!("/entries/{}", source["id"].as_str().unwrap());
    let mut content = draft();
    content["grammar_structures"] = json!([{
        "id":Uuid::now_v7(),
        "part_of_speech":"noun",
        "variants":[
            {"id":Uuid::now_v7(),"locale":"en-GB","content":{"version":2,"text":"target","annotations":[]},"form_links":[{
                "id":Uuid::now_v7(),
                "source_segments":[{"start":0,"end":6,"surface":"target"}],
                "target_source":"lexicon_next",
                "target_word_id":target_entry,
                "target_pos_id":target_pos,
                "target_form_id":target_form,
                "target_variant_id":target_variant,
                "target_dialect":"common"
            }]},
            {"id":Uuid::now_v7(),"locale":"en-US","content":{"version":2,"text":"target","annotations":[]}}
        ]
    }]);
    assert_eq!(
        call_grammar_links(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":content}),
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[sqlx::test]
async fn grammar_links_reject_v3_targets_without_form_topology(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool, EDIT).await;
    let target_entry = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,revision,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word',1,'{}',$2,$2)")
        .bind(target_entry)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let target_pos = Uuid::now_v7();
    let target_form = Uuid::now_v7();
    let target_variant = Uuid::now_v7();
    for (id, node_type) in [
        (target_pos, "pos"),
        (target_form, "form_slot"),
        (target_variant, "form_variant"),
    ] {
        sqlx::query("INSERT INTO lexicon.nodes(id,entry_id,node_type) VALUES($1,$2,$3)")
            .bind(id)
            .bind(target_entry)
            .bind(node_type)
            .execute(&pool)
            .await
            .unwrap();
    }
    let source = create(&state, actor).await;
    let path = format!("/entries/{}", source["id"].as_str().unwrap());
    let mut content = draft();
    content["grammar_structures"] = json!([{
        "id":Uuid::now_v7(),
        "part_of_speech":"noun",
        "variants":[
            {"id":Uuid::now_v7(),"locale":"en-GB","content":{"version":2,"text":"target","annotations":[]},"form_links":[{
                "id":Uuid::now_v7(),
                "source_segments":[{"start":0,"end":6,"surface":"target"}],
                "target_word_id":target_entry,
                "target_pos_id":target_pos,
                "target_form_id":target_form,
                "target_variant_id":target_variant,
                "target_dialect":"common"
            }]},
            {"id":Uuid::now_v7(),"locale":"en-US","content":{"version":2,"text":"target","annotations":[]}}
        ]
    }]);
    assert_eq!(
        call_grammar_links(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":content}),
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[sqlx::test]
async fn grammar_links_reject_publication_targets_from_different_form_branches(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool, EDIT).await;
    let target_entry = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,revision,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word',1,'{}',$2,$2)")
        .bind(target_entry)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let publication = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) VALUES($1,$2,1,1,3,'{}',$3,$4)")
        .bind(publication)
        .bind(target_entry)
        .bind(b"publication-topology".as_slice())
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let target_pos = Uuid::now_v7();
    let other_pos = Uuid::now_v7();
    let target_form = Uuid::now_v7();
    let target_variant = Uuid::now_v7();
    for (id, node_type, parent) in [
        (target_pos, "pos", None),
        (other_pos, "pos", None),
        (target_form, "concrete_form", Some(other_pos)),
        (target_variant, "form_variant", Some(target_form)),
    ] {
        sqlx::query(
            "INSERT INTO lexicon.nodes(id,entry_id,node_type,parent_node_id) VALUES($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(target_entry)
        .bind(node_type)
        .bind(parent)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO lexicon.entry_publication_nodes(publication_id,entry_id,node_id,node_type) VALUES($1,$2,$3,$4)")
            .bind(publication)
            .bind(target_entry)
            .bind(id)
            .bind(node_type)
            .execute(&pool)
            .await
            .unwrap();
    }
    let source = create(&state, actor).await;
    let path = format!("/entries/{}", source["id"].as_str().unwrap());
    let mut content = draft();
    content["grammar_structures"] = json!([{
        "id":Uuid::now_v7(),
        "part_of_speech":"noun",
        "variants":[
            {"id":Uuid::now_v7(),"locale":"en-GB","content":{"version":2,"text":"target","annotations":[]},"form_links":[{
                "id":Uuid::now_v7(),
                "source_segments":[{"start":0,"end":6,"surface":"target"}],
                "target_word_id":target_entry,
                "target_publication_id":publication,
                "target_pos_id":target_pos,
                "target_form_id":target_form,
                "target_variant_id":target_variant,
                "target_dialect":"common"
            }]},
            {"id":Uuid::now_v7(),"locale":"en-US","content":{"version":2,"text":"target","annotations":[]}}
        ]
    }]);
    assert_eq!(
        call_grammar_links(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":content}),
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

async fn call_grammar_audio(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    call_audio_client(
        state,
        Some(actor),
        method,
        path,
        body,
        (true, true, true),
        true,
    )
    .await
}

#[sqlx::test]
async fn grammar_audio_round_trips_preserves_legacy_and_owns_assets_with_forms(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let other_actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let (_, old_caps) = call_grammar(&state, actor, "GET", "/capabilities", Value::Null).await;
    assert!(old_caps.get("grammar_audio").is_none());
    let (_, caps) = call_grammar_audio(&state, actor, "GET", "/capabilities", Value::Null).await;
    assert_eq!(caps["grammar_audio"], true);
    let mut assets = Vec::new();
    for owner in [actor, actor, other_actor] {
        let asset = Uuid::now_v7();
        sqlx::query("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id) VALUES($1,$2,$3,'audio/mpeg',3,'en-GB','female','fixture.mp3',$4)")
            .bind(asset).bind(format!("local-fixture/{asset}")).bind(format!("local-fixture/source/{asset}")).bind(owner).execute(&pool).await.unwrap();
        let metadata: Value = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'locale',locale,'gender',gender,'content_type',content_type,'size_bytes',size_bytes,'duration_ms',duration_ms,'original_name',original_name,'created_at',created_at,'is_default',true) FROM lexicon.audio_assets WHERE id=$1")
            .bind(asset).fetch_one(&pool).await.unwrap();
        assets.push(
            serde_json::to_value(
                serde_json::from_value::<tsz_rust::lexicon::audio_assets::dto::AudioAsset>(
                    metadata,
                )
                .unwrap(),
            )
            .unwrap(),
        );
    }
    let mut content = entry["draft"].clone();
    let mut editor = form_editor();
    editor["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["audio_assets"] =
        json!([assets[0]]);
    content["form_editor"] = editor;
    let mut forged = assets[1].clone();
    forged["original_name"] = json!("forged.mp3");
    content["grammar_structures"] = json!([{"id":Uuid::now_v7(),"part_of_speech":"noun","sub_pos":"N-COUNT","variants":[{
        "id":Uuid::now_v7(),"locale":"en-GB","content":{"version":2,"text":"a job","annotations":[]},
        "voice_profile":{"voices":[{"voice_id":"saved-voice","enabled":false,"rate_percent":25}]},
        "audio_assets":[forged]
    }]}]);
    let (status, saved) = call_grammar_audio(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["draft"]["grammar_structures"][0]["variants"][0]["audio_assets"][0],
        assets[1]
    );
    let (_, reopened) = call_grammar_audio(&state, actor, "GET", &path, Value::Null).await;
    assert_eq!(reopened["draft"], saved["draft"]);
    let (_, projected) = call_grammar_audio(
        &state,
        actor,
        "GET",
        &format!("{path}/view?target_locale=en-GB&medium_locale=zh-Hans"),
        Value::Null,
    )
    .await;
    assert_eq!(
        projected["grammar_structures"],
        saved["draft"]["grammar_structures"]
    );
    let (_, old) = call_grammar(&state, actor, "GET", &path, Value::Null).await;
    assert!(
        old["draft"]["grammar_structures"][0]["variants"][0]
            .get("voice_profile")
            .is_none()
    );
    let (status, _) = call_grammar(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":old["draft"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, retained) = call_grammar_audio(&state, actor, "GET", &path, Value::Null).await;
    assert_eq!(retained["draft"], saved["draft"]);
    for invalid_asset in [assets[2].clone(), {
        let mut value = assets[1].clone();
        value["id"] = json!(Uuid::now_v7());
        value
    }] {
        let mut invalid = retained["draft"].clone();
        invalid["grammar_structures"][0]["variants"][0]["audio_assets"] = json!([invalid_asset]);
        let (status, _) = call_grammar_audio(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":invalid}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let mut invalid = retained["draft"].clone();
    invalid["grammar_structures"][0]["variants"][0]["voice_profile"]["voices"][0]["rate_percent"] =
        json!(101);
    assert_eq!(
        call_grammar_audio(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":invalid})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut forged_generation = retained["draft"].clone();
    forged_generation["grammar_structures"][0]["variants"][0]["voice_profile"]["voices"] = json!([{
        "voice_id":"saved-voice", "enabled":false, "rate_percent":0,
        "version_id":Uuid::now_v7(), "version_name":"未生成的版本", "locale":"en-GB", "audio_asset_id":assets[1]["id"]
    }]);
    assert_eq!(
        call_grammar_audio(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":forged_generation})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut wrong_accent = retained["draft"].clone();
    wrong_accent["grammar_structures"][0]["variants"][0]["locale"] = json!("en-US");
    wrong_accent["grammar_structures"][0]["variants"][0]["id"] = json!(Uuid::now_v7());
    assert_eq!(
        call_grammar_audio(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":wrong_accent})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let another = create(&state, actor).await;
    let mut cloned = another["draft"].clone();
    cloned["grammar_structures"] = content["grammar_structures"].clone();
    cloned["grammar_structures"][0]["id"] = json!(Uuid::now_v7());
    cloned["grammar_structures"][0]["variants"][0]["id"] = json!(Uuid::now_v7());
    assert_eq!(
        call_grammar_audio(
            &state,
            actor,
            "PUT",
            &format!("/entries/{}", another["id"].as_str().unwrap()),
            json!({"expected_revision":1,"draft":cloned})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut changed = old["draft"].clone();
    changed["grammar_structures"][0]["variants"][0]["content"]["text"] = json!("a new job");
    assert_eq!(
        call_grammar(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":changed})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, changed) = call_grammar_audio(&state, actor, "GET", &path, Value::Null).await;
    assert!(
        changed["draft"]["grammar_structures"][0]["variants"][0]
            .get("audio_assets")
            .is_none()
    );
    assert_eq!(
        changed["draft"]["grammar_structures"][0]["variants"][0]["voice_profile"]["voices"][0]["rate_percent"],
        25
    );
    let references: Vec<Uuid> = sqlx::query_scalar(
        "SELECT asset_id FROM lexicon_next.audio_asset_references WHERE entry_id=$1",
    )
    .bind(Uuid::parse_str(entry["id"].as_str().unwrap()).unwrap())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        references,
        vec![Uuid::parse_str(assets[0]["id"].as_str().unwrap()).unwrap()]
    );
}

#[sqlx::test]
async fn sense_metadata_roundtrips_and_legacy_edits_preserve_values(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let reader = admin(&pool, &["words.access"]).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let mut content = draft();
    content["senses"][0]["level"] = json!("B2");
    content["senses"][0]["frequency"] = json!("95.50");
    content["senses"][0]["depends_on_context"] = json!(true);
    let request = json!({"expected_revision":1,"draft":content});
    assert_eq!(
        call(&state, Some(actor), "PUT", &path, request.clone())
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        call_editor_client(
            &state,
            Some(reader),
            "PUT",
            &path,
            request.clone(),
            (false, false, false),
            (false, true)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, saved) = call_editor_client(
        &state,
        Some(actor),
        "PUT",
        &path,
        request.clone(),
        (false, false, false),
        (false, true),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["draft"], content);
    assert_eq!(
        call_editor_client(
            &state,
            Some(actor),
            "PUT",
            &path,
            request,
            (false, false, false),
            (false, true)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, read) = call_editor_client(
        &state,
        Some(actor),
        "GET",
        &path,
        Value::Null,
        (false, false, false),
        (false, true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["draft"], content);
    let view_path = format!("{path}/view?target_locale=en-US&medium_locale=zh-Hans");
    let (_, view) = call_editor_client(
        &state,
        Some(actor),
        "GET",
        &view_path,
        Value::Null,
        (false, false, false),
        (false, true),
    )
    .await;
    assert_eq!(view["senses"][0]["level"], "B2");
    assert_eq!(view["senses"][0]["frequency"], "95.50");
    assert_eq!(view["senses"][0]["depends_on_context"], true);
    for endpoint in [&path, &view_path] {
        let (_, legacy) = call(&state, Some(actor), "GET", endpoint, Value::Null).await;
        let senses = if endpoint == &path {
            &legacy["draft"]["senses"]
        } else {
            &legacy["senses"]
        };
        for field in ["level", "frequency", "depends_on_context"] {
            assert!(senses[0].get(field).is_none());
        }
    }
    let (_, legacy) = call(&state, Some(actor), "GET", &path, Value::Null).await;
    let mut old_content = legacy["draft"].clone();
    old_content["headword"] = json!("centre legacy edit");
    assert_eq!(
        call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":old_content})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, read) = call_editor_client(
        &state,
        Some(actor),
        "GET",
        &path,
        Value::Null,
        (false, false, false),
        (false, true),
    )
    .await;
    assert_eq!(read["draft"]["senses"][0], content["senses"][0]);
    // The list and counters use only the current saved revision, never historical classifications.
    let (_, page) = call(
        &state,
        Some(actor),
        "GET",
        "/workspace/entries?level=B2",
        Value::Null,
    )
    .await;
    assert_eq!(page["pagination"]["total"], 1);
    let (_, stats) = call(
        &state,
        Some(actor),
        "GET",
        "/workspace/stats?level=B2",
        Value::Null,
    )
    .await;
    assert_eq!(stats["total"], 1);
    let mut updated = read["draft"].clone();
    updated["senses"][0]["level"] = json!("C1");
    updated["senses"][0]["depends_on_context"] = json!(false);
    updated["senses"][0]
        .as_object_mut()
        .unwrap()
        .remove("frequency");
    let (_, saved) = call_editor_client(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":updated}),
        (false, false, false),
        (false, true),
    )
    .await;
    assert_eq!(saved["draft"], updated);
    assert!(saved["draft"]["senses"][0].get("frequency").is_none());
    let (_, page) = call(
        &state,
        Some(actor),
        "GET",
        "/workspace/entries?level=B2",
        Value::Null,
    )
    .await;
    assert_eq!(page["pagination"]["total"], 0);
    let (_, page) = call(
        &state,
        Some(actor),
        "GET",
        "/workspace/entries?level=C1",
        Value::Null,
    )
    .await;
    assert_eq!(page["pagination"]["total"], 1);
    let (_, caps) = call(&state, Some(actor), "GET", "/capabilities", Value::Null).await;
    assert!(caps.get("sense_metadata").is_none());
    let (_, caps) = call_editor_client(
        &state,
        Some(actor),
        "GET",
        "/capabilities",
        Value::Null,
        (false, false, false),
        (false, true),
    )
    .await;
    assert_eq!(caps["sense_metadata"], true);
}

async fn equivalent_call(
    state: &AppState,
    actor: Option<Uuid>,
    method: &str,
    path: &str,
    body: Value,
    negotiated: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if negotiated {
        req = req.header("x-tsz-next-equivalents", "v1");
    }
    if let Some(actor) = actor {
        req = req.header(
            "authorization",
            format!(
                "Bearer {}",
                state.admin_token_manager.generate(actor, "admin").unwrap()
            ),
        );
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
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
async fn equivalent_taxonomy(pool: &PgPool, kind: &str, code: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO catalog.lexicon_taxonomies(id,kind,code,name_zh,name_en) VALUES($1,$2,$3,$3,$3)")
        .bind(id).bind(kind).bind(code).execute(pool).await.unwrap();
    id
}

#[sqlx::test]
async fn equivalent_metadata_roundtrip_legacy_preservation_and_reference_protection(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(
        &pool,
        &[
            "words.access",
            "words.create",
            "words.edit",
            "lexicon_settings.access",
            "lexicon_settings.delete",
        ],
    )
    .await;
    let register = equivalent_taxonomy(&pool, "registers", "spoken").await;
    let domain = equivalent_taxonomy(&pool, "domains", "business").await;
    let entry = create(&state, actor).await;
    let path = format!("{ROOT}/entries/{}", entry["id"].as_str().unwrap());
    let mut content = draft();
    content["senses"][0]["equivalents"][0]["metadata"] = json!({"register_ids":[register],"domain_ids":[domain],"commonness":[{"score":"95"},{"domain_id":domain,"score":"80.0","assessed_at":"2000-01-01T00:00:00Z"}]});
    content["senses"][0]["no_equivalents"] = json!(["zh-Hant"]);
    // The fixture also has a Traditional equivalent; remove it before declaring none.
    content["senses"][0]["equivalents"]
        .as_array_mut()
        .unwrap()
        .retain(|r| r["locale"] != "zh-Hant");
    let (status, saved) = equivalent_call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let metadata = saved["draft"]["senses"][0]["equivalents"][0]["metadata"].clone();
    assert_eq!(metadata["commonness"][0]["score"], "95.0");
    assert_ne!(
        metadata["commonness"][1]["assessed_at"],
        "2000-01-01T00:00:00Z"
    );
    let (_, read) = equivalent_call(&state, Some(actor), "GET", &path, Value::Null, true).await;
    assert_eq!(read["draft"], saved["draft"]);
    let projection: Value =
        sqlx::query_scalar("SELECT metadata FROM lexicon_next.equivalents WHERE id=$1")
            .bind(
                Uuid::parse_str(
                    saved["draft"]["senses"][0]["equivalents"][0]["id"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap(),
            )
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(projection, metadata);
    let (_, old) = equivalent_call(&state, Some(actor), "GET", &path, Value::Null, false).await;
    assert!(
        old["draft"]["senses"][0]["equivalents"][0]
            .get("metadata")
            .is_none()
    );
    assert!(old["draft"]["senses"][0].get("no_equivalents").is_none());
    let mut old_draft = old["draft"].clone();
    old_draft["senses"][0]["equivalents"][0]["text"] = json!("核心");
    let (status, old_saved) = equivalent_call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":old_draft}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{old_saved}");
    let (_, reopened) = equivalent_call(&state, Some(actor), "GET", &path, Value::Null, true).await;
    assert_eq!(
        reopened["draft"]["senses"][0]["equivalents"][0]["metadata"],
        metadata
    );
    assert_eq!(
        reopened["draft"]["senses"][0]["no_equivalents"],
        json!(["zh-Hant"])
    );
    let (_, view) = equivalent_call(
        &state,
        Some(actor),
        "GET",
        &format!("{path}/view?target_locale=en-GB&medium_locale=zh-Hant"),
        Value::Null,
        true,
    )
    .await;
    assert_eq!(view["senses"][0]["no_equivalents"], true);
    assert_eq!(view["senses"][0]["missing_equivalents"], false);
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261115100000_lexicon_next_equivalent_metadata.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err(),
        "used metadata must refuse unsafe rollback"
    );
    let deletion = format!("/api/v1/admin/lexicon/taxonomies/domains/{domain}?base_revision=1");
    let (status, rejected) =
        equivalent_call(&state, Some(actor), "DELETE", &deletion, Value::Null, false).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(rejected["code"], "taxonomy_conflict");
    // A capable client can clear, but retained historical references still protect the directory.
    let mut cleared = reopened["draft"].clone();
    cleared["senses"][0]["equivalents"][0]
        .as_object_mut()
        .unwrap()
        .remove("metadata");
    cleared["senses"][0]["no_equivalents"] = json!([]);
    let (status, cleared) = equivalent_call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":cleared}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert!(
        cleared["draft"]["senses"][0]["equivalents"][0]
            .get("metadata")
            .is_none()
    );
    let (status, _) =
        equivalent_call(&state, Some(actor), "DELETE", &deletion, Value::Null, false).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = equivalent_call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":cleared["draft"]}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn equivalent_catalog_permission_and_invalid_metadata(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool, &["words.access", "words.create", "words.edit"]).await;
    let denied = admin(&pool, &[]).await;
    let register = equivalent_taxonomy(&pool, "registers", "spoken").await;
    for i in 0..105 {
        equivalent_taxonomy(&pool, "domains", &format!("domain_{i}")).await;
    }
    let catalog = format!("{ROOT}/equivalent-catalog");
    let (status, options) =
        equivalent_call(&state, Some(actor), "GET", &catalog, Value::Null, true).await;
    assert_eq!(status, StatusCode::OK, "{options}");
    assert_eq!(options["domains"].as_array().unwrap().len(), 105);
    assert_eq!(options["registers"][0]["id"], register.to_string());
    for (who, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(denied), StatusCode::FORBIDDEN),
    ] {
        assert_eq!(
            equivalent_call(&state, who, "GET", &catalog, Value::Null, true)
                .await
                .0,
            expected
        );
    }
    let entry = create(&state, actor).await;
    let path = format!("{ROOT}/entries/{}", entry["id"].as_str().unwrap());
    let metadata =
        json!({"register_ids":[register],"domain_ids":[],"commonness":[{"score":"0.1"}]});
    for patch in [
        json!({"register_ids":[Uuid::now_v7()]}),
        json!({"domain_ids":[register]}),
        json!({"commonness":[{"score":"0"}]}),
        json!({"commonness":[{"score":"100"}]}),
        json!({"commonness":[{"score":"1.01"}]}),
        json!({"commonness":[{"score":"1"},{"score":"2"}]}),
        json!({"commonness":null}),
    ] {
        let mut content = draft();
        let mut bad = metadata.clone();
        for (k, v) in patch.as_object().unwrap() {
            bad[k] = v.clone();
        }
        content["senses"][0]["equivalents"][0]["metadata"] = bad;
        let (status, error) = equivalent_call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":content}),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    }
    let mut content = draft();
    content["senses"][0]["equivalents"][0]["metadata"] = metadata;
    assert_eq!(
        equivalent_call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":content}),
            false
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        equivalent_call(
            &state,
            Some(denied),
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":draft()}),
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut duplicates = draft();
    let mut second = duplicates["senses"][0]["equivalents"][0].clone();
    second["id"] = json!(Uuid::now_v7());
    duplicates["senses"][0]["equivalents"]
        .as_array_mut()
        .unwrap()
        .push(second);
    assert_eq!(
        equivalent_call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":duplicates}),
            true
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut content = draft();
    content["senses"][0]["equivalents"][0]["metadata"] =
        json!({"register_ids":[],"domain_ids":[],"commonness":[{"score":"99.9"}]});
    content["senses"][0]["equivalents"][1]["metadata"] =
        json!({"register_ids":[],"domain_ids":[],"commonness":[{"score":"0.1"}]});
    let (status, saved) = equivalent_call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
}

#[sqlx::test]
async fn equivalent_save_racing_taxonomy_delete_cannot_create_dangling_reference(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(
        &pool,
        &[
            "words.access",
            "words.create",
            "words.edit",
            "lexicon_settings.access",
            "lexicon_settings.delete",
        ],
    )
    .await;
    let domain = equivalent_taxonomy(&pool, "domains", "race_domain").await;
    let entry = create(&state, actor).await;
    let path = format!("{ROOT}/entries/{}", entry["id"].as_str().unwrap());
    let deletion = format!("/api/v1/admin/lexicon/taxonomies/domains/{domain}?base_revision=1");
    let mut content = draft();
    content["senses"][0]["equivalents"][0]["metadata"] =
        json!({"register_ids":[],"domain_ids":[domain],"commonness":[]});
    let ((save_status, save_body), (delete_status, delete_body)) = tokio::join!(
        equivalent_call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":content}),
            true
        ),
        equivalent_call(&state, Some(actor), "DELETE", &deletion, Value::Null, false)
    );
    assert!(
        matches!(
            (save_status, delete_status),
            (StatusCode::OK, StatusCode::CONFLICT)
                | (StatusCode::UNPROCESSABLE_ENTITY, StatusCode::NO_CONTENT)
        ),
        "save {save_status} {save_body}; delete {delete_status} {delete_body}"
    );
    let dangling: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon_next.equivalent_taxonomy_refs r LEFT JOIN catalog.lexicon_taxonomies t ON t.id=r.taxonomy_id WHERE t.id IS NULL").fetch_one(&pool).await.unwrap();
    assert_eq!(dangling, 0);
}

async fn definition_call(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
    details: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("{ROOT}{path}"))
        .header("content-type", "application/json")
        .header("x-tsz-next-forms", "v1")
        .header("x-tsz-next-grammar", "v1")
        .header("x-tsz-next-grammar-audio", "v1")
        .header("x-tsz-next-sense-metadata", "v1")
        .header("x-tsz-next-equivalents", "v1")
        .header("x-tsz-next-sense-groups", "v1")
        .header("x-tsz-next-medium-ownership", "v1")
        .header(
            "authorization",
            format!(
                "Bearer {}",
                state.admin_token_manager.generate(actor, "admin").unwrap()
            ),
        );
    if details {
        req = req.header("x-tsz-next-definitions", "v1");
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn definition_recording(pool: &PgPool, actor: Uuid, locale: &str) -> Value {
    let asset = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id) VALUES($1,$2,$3,'audio/mpeg',3,$4,'female','fixture.mp3',$5)")
        .bind(asset).bind(format!("local-fixture/{asset}")).bind(format!("local-fixture/source/{asset}")).bind(locale).bind(actor).execute(pool).await.unwrap();
    let metadata: Value = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'locale',locale,'gender',gender,'content_type',content_type,'size_bytes',size_bytes,'duration_ms',duration_ms,'original_name',original_name,'created_at',created_at) FROM lexicon.audio_assets WHERE id=$1")
        .bind(asset).fetch_one(pool).await.unwrap();
    serde_json::to_value(
        serde_json::from_value::<tsz_rust::lexicon::audio_assets::dto::AudioAsset>(metadata)
            .unwrap(),
    )
    .unwrap()
}

#[sqlx::test]
async fn definition_details_roundtrip_legacy_edits_language_views_and_conflicts(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let reader = admin(&pool, &["words.access"]).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let (_, old_caps) =
        definition_call(&state, actor, "GET", "/capabilities", Value::Null, false).await;
    assert!(old_caps.get("definition_details").is_none());
    let (_, caps) = definition_call(&state, actor, "GET", "/capabilities", Value::Null, true).await;
    assert_eq!(caps["definition_details"], true);
    let asset = definition_recording(&pool, actor, "en-GB").await;
    let mut content = draft();
    content["grammar_structures"] = json!([]);
    content["senses"][0]["level"] = json!("C2");
    content["senses"][0]["definitions"][0]["level"] = json!("A1");
    content["senses"][0]["definitions"][0]["grammar_unbound"] = json!(true);
    content["senses"][0]["definitions"][0]["content"] = json!({"version":2,"text":content["senses"][0]["definitions"][0]["text"],"annotations":[{"type":"pause","at":3,"duration_ms":250}]});
    content["senses"][0]["definitions"][0]["audio_assets"] = json!([asset]);
    let mut second = content["senses"][0]["definitions"][0].clone();
    second["id"] = json!(Uuid::now_v7());
    second.as_object_mut().unwrap().remove("audio_assets");
    content["senses"][0]["definitions"]
        .as_array_mut()
        .unwrap()
        .push(second);
    content["senses"][0]["definitions"][2]["level"] = json!("B2");
    let (status, _) = definition_call(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, saved) = definition_call(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, read) = definition_call(&state, actor, "GET", &path, Value::Null, true).await;
    assert_eq!(read["draft"], saved["draft"]);
    let (_, view) = definition_call(
        &state,
        actor,
        "GET",
        &format!("{path}/view?target_locale=en-GB&medium_locale=zh-Hans"),
        Value::Null,
        true,
    )
    .await;
    assert_eq!(
        view["senses"][0]["target_definitions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(view["senses"][0]["target_definitions"][0]["level"], "A1");
    assert_eq!(view["senses"][0]["medium_definitions"][0]["level"], "B2");
    assert!(
        !view["grammar_issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["node_id"] == content["senses"][0]["definitions"][0]["id"])
    );
    let (_, us) = definition_call(
        &state,
        actor,
        "GET",
        &format!("{path}/view?target_locale=en-US&medium_locale=zh-Hans"),
        Value::Null,
        true,
    )
    .await;
    assert_eq!(
        us["senses"][0]["target_definitions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        us["senses"][0]["target_definitions"][0]
            .get("level")
            .is_none()
    );
    let (_, old) = definition_call(&state, actor, "GET", &path, Value::Null, false).await;
    for field in [
        "level",
        "grammar_unbound",
        "content",
        "voice_profile",
        "audio_assets",
    ] {
        assert!(
            old["draft"]["senses"][0]["definitions"][0]
                .get(field)
                .is_none(),
            "{field}"
        );
    }
    let (_, old_view) = definition_call(
        &state,
        actor,
        "GET",
        &format!("{path}/view?target_locale=en-GB&medium_locale=zh-Hans"),
        Value::Null,
        false,
    )
    .await;
    assert!(
        old_view["senses"][0]["target_definitions"][0]
            .get("content")
            .is_none()
    );
    assert_eq!(
        definition_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":old["draft"]}),
            false
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, retained) = definition_call(&state, actor, "GET", &path, Value::Null, true).await;
    assert_eq!(retained["draft"], saved["draft"]);
    assert_eq!(
        definition_call(
            &state,
            reader,
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":retained["draft"]}),
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        definition_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":content}),
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut legacy = old["draft"].clone();
    legacy["senses"][0]["definitions"][0]["text"] = json!("a new centre");
    legacy["senses"][0]["definitions"]
        .as_array_mut()
        .unwrap()
        .swap(0, 4);
    assert_eq!(
        definition_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":legacy}),
            false
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, changed) = definition_call(&state, actor, "GET", &path, Value::Null, true).await;
    assert_eq!(changed["draft"]["forms"], saved["draft"]["forms"]);
    assert_eq!(
        changed["draft"]["senses"][0]["equivalents"],
        saved["draft"]["senses"][0]["equivalents"]
    );
    assert_eq!(
        changed["draft"]["senses"][0]["definitions"][4]["id"],
        saved["draft"]["senses"][0]["definitions"][0]["id"]
    );
    assert_eq!(
        changed["draft"]["senses"][0]["definitions"][4]["level"],
        "A1"
    );
    assert!(
        changed["draft"]["senses"][0]["definitions"][4]
            .get("content")
            .is_none()
    );
    assert!(
        changed["draft"]["senses"][0]["definitions"][4]
            .get("audio_assets")
            .is_none()
    );
    let refs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon_next.audio_asset_references WHERE entry_id=$1",
    )
    .bind(Uuid::parse_str(entry["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(refs, 0);
}

#[sqlx::test]
async fn definition_details_reject_invalid_assets_generation_sources_and_archived_writes(
    pool: PgPool,
) {
    let actor = admin(
        &pool,
        &[
            "words.access",
            "words.create",
            "words.edit",
            "words.archive",
        ],
    )
    .await;
    let other = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let recording = definition_recording(&pool, actor, "en-GB").await;
    let foreign = definition_recording(&pool, other, "en-GB").await;
    let mut forged_us = definition_recording(&pool, actor, "en-US").await;
    forged_us["locale"] = json!("en-GB");
    let mut missing = recording.clone();
    missing["id"] = json!(Uuid::now_v7());
    let mut content = draft();
    content["senses"][0]["definitions"][0]["content"] =
        json!({"version":2,"text":content["senses"][0]["definitions"][0]["text"],"annotations":[]});
    for asset in [foreign, forged_us, missing] {
        let mut invalid = content.clone();
        invalid["senses"][0]["definitions"][0]["audio_assets"] = json!([asset]);
        assert_eq!(
            definition_call(
                &state,
                actor,
                "PUT",
                &path,
                json!({"expected_revision":1,"draft":invalid}),
                true
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let mut generated = content.clone();
    generated["senses"][0]["definitions"][0]["voice_profile"] = json!({"voices":[{
        "voice_id":"saved-voice","enabled":false,"rate_percent":0,"version_id":Uuid::now_v7(),"version_name":"not generated","locale":"en-GB","audio_asset_id":recording["id"]
    }]});
    assert_eq!(
        definition_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":generated}),
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // Source mismatches must be rejected even when an asset has a ready generation row.
    sqlx::query("INSERT INTO lexicon.audio_generations(asset_id,request_id,created_by_admin_id,input_hash,source,voice,resolved_content,state) VALUES($1,$2,$3,$4,$5,$6,$7,'ready')")
        .bind(Uuid::parse_str(recording["id"].as_str().unwrap()).unwrap()).bind(Uuid::now_v7()).bind(actor).bind(vec![0u8;32])
        .bind(json!({"kind":"text","content":{"version":2,"text":"different source","annotations":[]},"form_links":[]}))
        .bind(json!({})).bind(json!({})).execute(&pool).await.unwrap();
    assert_eq!(
        definition_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":generated}),
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut disguised = content.clone();
    disguised["senses"][0]["definitions"][0]["audio_assets"] = json!([recording]);
    assert_eq!(
        definition_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":1,"draft":disguised}),
            true
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (_, unchanged) = definition_call(&state, actor, "GET", &path, Value::Null, true).await;
    assert_eq!(unchanged["revision"], 1);
    let (_, list) = call_scoped(
        &state,
        actor,
        "GET",
        "/workspace/entries?medium_locale=zh-Hans",
        Value::Null,
    )
    .await;
    let row = &list["items"][0];
    assert_eq!(call_scoped(&state, actor, "POST", "/workspace/commands", json!({"request_id":Uuid::now_v7(),"action":"archive","items":[{"entry_id":entry["id"],"base_revision":1,"base_lifecycle_revision":row["lifecycle_revision"]}]})).await.0, StatusCode::OK);
    let (status, error) = definition_call(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "entry_archived");
}

#[sqlx::test]
async fn definition_details_common_english_invalid_grammar_and_verified_generation_binding(
    pool: PgPool,
) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let asset = definition_recording(&pool, actor, "en-GB").await;
    let version = Uuid::now_v7();
    let grammar = Uuid::now_v7();
    let rich = json!({"version":2,"text":"the middle","annotations":[]});
    // A database fixture proves source validation, not provider synthesis (batch 3).
    sqlx::query("INSERT INTO lexicon.audio_generations(asset_id,request_id,created_by_admin_id,input_hash,source,voice,resolved_content,state) VALUES($1,$2,$3,$4,$5,$6,$7,'ready')")
        .bind(Uuid::parse_str(asset["id"].as_str().unwrap()).unwrap()).bind(Uuid::now_v7()).bind(actor).bind(vec![0u8;32])
        .bind(json!({"kind":"text","content":rich,"form_links":[]}))
        .bind(json!({"version_id":version,"voice_id":"saved-voice","locale":"en-GB","rate_percent":0,"pitch_semitones":0,"volume_percent":100,"style":""}))
        .bind(&rich).execute(&pool).await.unwrap();
    let mut content = draft();
    content["grammar_structures"] = json!([{"id":grammar,"part_of_speech":"adjective","variants":[{
        "id":Uuid::now_v7(),"locale":"en","content":{"version":2,"text":"a common structure","annotations":[]}
    }]}]);
    content["senses"][0]["definitions"] = json!([
        {"id":Uuid::now_v7(),"locale":"en","style":"definition","text":"the middle","level":"A2","grammar_structure_id":grammar,
            "content":rich,"voice_profile":{"voices":[{"voice_id":"saved-voice","enabled":false,"rate_percent":0,"version_id":version,"version_name":"verified fixture","locale":"en-GB","audio_asset_id":asset["id"]}]}},
        {"id":Uuid::now_v7(),"locale":"zh-Hans","style":"sentence","text":"这是中心。","grammar_unbound":true}
    ]);
    let (status, saved) = definition_call(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    for target in ["en-GB", "en-US"] {
        let (_, view) = definition_call(
            &state,
            actor,
            "GET",
            &format!("{path}/view?target_locale={target}&medium_locale=zh-Hans"),
            Value::Null,
            true,
        )
        .await;
        let row = &view["senses"][0]["target_definitions"][0];
        assert_eq!(row["locale"], "en");
        assert_eq!(row["grammar_structure_id"], json!(grammar));
        assert!(
            view["grammar_issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|issue| issue["node_id"] == row["id"])
        );
    }
    let (_, old_view) = definition_call(
        &state,
        actor,
        "GET",
        &format!("{path}/view?target_locale=en-GB&medium_locale=zh-Hans"),
        Value::Null,
        false,
    )
    .await;
    assert!(
        old_view["senses"][0]["target_definitions"][0]
            .get("grammar_structure_id")
            .is_none()
    );
    let mut changed = saved["draft"].clone();
    changed["senses"][0]["definitions"][0]["text"] = json!("a different middle");
    changed["senses"][0]["definitions"][0]["content"]["text"] = json!("a different middle");
    assert_eq!(
        definition_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":changed}),
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (_, old) = definition_call(&state, actor, "GET", &path, Value::Null, false).await;
    let mut legacy = old["draft"].clone();
    legacy["senses"][0]["definitions"][0]["text"] = json!("a different middle");
    assert_eq!(
        definition_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":legacy}),
            false
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, changed) = definition_call(&state, actor, "GET", &path, Value::Null, true).await;
    assert_eq!(
        changed["draft"]["senses"][0]["definitions"][0]["content"]["text"],
        "a different middle"
    );
    assert!(
        changed["draft"]["senses"][0]["definitions"][0]["voice_profile"]["voices"][0]
            .get("audio_asset_id")
            .is_none()
    );
    let refs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon_next.audio_asset_references WHERE entry_id=$1",
    )
    .bind(Uuid::parse_str(entry["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(refs, 0);
}

async fn sentence_client(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    sentence_client_details(state, actor, method, path, body, false).await
}
async fn sentence_client_details(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
    details: bool,
) -> (StatusCode, Value) {
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("{ROOT}{path}"))
                .header("content-type", "application/json")
                .header("x-tsz-next-sentences", "v1")
                .header(
                    "x-tsz-next-sentence-details",
                    if details { "v1" } else { "unknown" },
                )
                .header("x-tsz-next-forms", "v1")
                .header(
                    "authorization",
                    format!(
                        "Bearer {}",
                        state.admin_token_manager.generate(actor, "admin").unwrap()
                    ),
                )
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap(),
    )
}
async fn unified_sentence_client(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("{ROOT}{path}"))
                .header("content-type", "application/json")
                .header("x-tsz-next-sentences", "v1")
                .header("x-tsz-next-sentence-details", "v1")
                .header("x-tsz-next-sentence-drafts", "v1")
                .header(
                    "authorization",
                    format!(
                        "Bearer {}",
                        state.admin_token_manager.generate(actor, "admin").unwrap()
                    ),
                )
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap(),
    )
}

fn sentence_variant(locale: &str, medium: &str, text: &str) -> Value {
    json!({"id":Uuid::now_v7(),"locale":locale,"level":"A2","content":{"version":2,"text":text,"annotations":[]},"translations":[{"id":Uuid::now_v7(),"locale":medium,"style":"balanced_fluency","content":{"version":2,"text":"例句译文","annotations":[]}}]})
}
const SENTENCE_EDIT: &[&str] = &[
    "words.access",
    "words.create",
    "words.edit",
    "sentences.access",
    "sentences.create",
    "sentences.edit",
];

#[sqlx::test]
async fn next_sentence_details_persist_negotiate_validate_and_protect_taxonomies(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool, SENTENCE_EDIT).await;
    let register = equivalent_taxonomy(&pool, "registers", "sentence_spoken").await;
    let domain = equivalent_taxonomy(&pool, "domains", "sentence_business").await;
    let id = Uuid::now_v7();
    let source = json!({"id":Uuid::now_v7(),"source_type":"dictionary","title":"人工登记的资料","url":"https://example.com/entry","locator":"例句 2"});
    let mut uk = sentence_variant("en-GB", "zh-Hans", "It is in the centre.");
    uk["metadata"] = json!({"register_ids":[register],"domain_ids":[domain],"sources":[source]});
    uk["translations"][0]["metadata"] = json!({"freedom":33.4,"sources":[]});
    let input = json!({"id":id,"target_language":"en","target_locale":"en-GB","medium_locale":"zh-Hans","variant":uk});
    assert_eq!(
        sentence_client(&state, actor, "POST", "/sentences", input.clone())
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (status, created) =
        sentence_client_details(&state, actor, "POST", "/sentences", input.clone(), true).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        created["draft"]["variants"][0]["translations"][0]["style"],
        "balanced_fluency"
    );
    assert_eq!(
        sentence_client_details(&state, actor, "POST", "/sentences", input, true)
            .await
            .1["revision"],
        1
    );
    let path = format!("/sentences/{id}");
    let old = sentence_client(&state, actor, "GET", &path, Value::Null)
        .await
        .1;
    assert!(old["draft"]["variants"][0].get("metadata").is_none());
    assert!(
        old["draft"]["variants"][0]["translations"][0]
            .get("metadata")
            .is_none()
    );
    let old_write = json!({"expected_revision":1,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":old["draft"]["variants"][0]});
    assert_eq!(
        sentence_client(&state, actor, "PUT", &path, old_write)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        sentence_client_details(&state, actor, "GET", &path, Value::Null, true)
            .await
            .1,
        created
    );
    let us = sentence_variant("en-US", "zh-Hans", "It is in the center.");
    let (status, saved) = sentence_client_details(&state,actor,"PUT",&path,json!({"expected_revision":1,"target_locale":"en-US","medium_locale":"zh-Hans","variant":us}),true).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["draft"]["variants"][0],
        created["draft"]["variants"][0]
    );
    let mut traditional = saved["draft"]["variants"][0].clone();
    traditional["translations"] =
        sentence_variant("en-GB", "zh-Hant", "unused")["translations"].clone();
    traditional["translations"][0]["metadata"] = json!({"freedom":100.0,"sources":[]});
    let (status,saved) = sentence_client_details(&state,actor,"PUT",&path,json!({"expected_revision":2,"target_locale":"en-GB","medium_locale":"zh-Hant","variant":traditional}),true).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["draft"]["variants"][0]["translations"][0]["style"],
        "adapted_creation"
    );
    assert_eq!(
        saved["draft"]["variants"][0]["translations"][1],
        created["draft"]["variants"][0]["translations"][0]
    );
    let stored: Value =
        sqlx::query_scalar("SELECT document FROM lexicon_next.sentences WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, saved["draft"]);
    let refs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon_next.sentence_taxonomy_refs WHERE sentence_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(refs, 2);
    assert!(
        sqlx::query("DELETE FROM catalog.lexicon_taxonomies WHERE id=$1")
            .bind(domain)
            .execute(&pool)
            .await
            .is_err()
    );
    for translation_metadata in [false, true] {
        let mut bad = uk.clone();
        if translation_metadata {
            bad["translations"][0]["metadata"] = Value::Null;
        } else {
            bad["metadata"] = Value::Null;
        }
        assert_eq!(sentence_client_details(&state, actor, "PUT", &path, json!({"expected_revision":3,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":bad}), true).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    }
    for invalid in [json!(33.33), json!(-0.1), json!(100.1)] {
        let mut bad = uk.clone();
        bad["translations"][0]["metadata"]["freedom"] = invalid;
        let (status, _) = sentence_client_details(&state,actor,"PUT",&path,json!({"expected_revision":3,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":bad}),true).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    for (field, value) in [("url", "javascript:alert(1)"), ("title", "")] {
        let mut bad = uk.clone();
        bad["metadata"]["sources"][0][field] = json!(value);
        assert_eq!(sentence_client_details(&state,actor,"PUT",&path,json!({"expected_revision":3,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":bad}),true).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let mut wrong_kind = uk.clone();
    wrong_kind["metadata"]["register_ids"] = json!([domain]);
    assert_eq!(sentence_client_details(&state,actor,"PUT",&path,json!({"expected_revision":3,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":wrong_kind}),true).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        sentence_client_details(&state, actor, "GET", &path, Value::Null, true)
            .await
            .1,
        saved
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261115140000_next_sentence_taxonomies.down.sql"
        ))
        .execute(&mut *tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    sqlx::query("DELETE FROM lexicon_next.sentences WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM lexicon_next.sentence_taxonomy_refs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261115140000_next_sentence_taxonomies.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261115140000_next_sentence_taxonomies.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
}
#[sqlx::test]
async fn next_sentences_scope_revision_permissions_and_idempotence(pool: PgPool) {
    let actor = admin(&pool, SENTENCE_EDIT).await;
    let reader = admin(&pool, &["sentences.access"]).await;
    let outsider = admin(&pool, SENTENCE_EDIT).await;
    let state = AppState::for_test(pool.clone());
    let id = Uuid::now_v7();
    let uk = sentence_variant("en-GB", "zh-Hans", "It is in the centre.");
    let input = json!({"id":id,"target_language":"en","target_locale":"en-GB","medium_locale":"zh-Hans","variant":uk});
    let (status, created) =
        sentence_client(&state, actor, "POST", "/sentences", input.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["revision"], 1);
    assert_eq!(
        sentence_client(&state, actor, "POST", "/sentences", input.clone())
            .await
            .1["id"],
        json!(id)
    );
    assert_eq!(
        sentence_client(&state, reader, "POST", "/sentences", input.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        sentence_client(&state, outsider, "POST", "/sentences", input.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut changed = input.clone();
    changed["variant"]["content"]["text"] = json!("Changed input");
    assert_eq!(
        sentence_client(&state, actor, "POST", "/sentences", changed)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let path = format!("/sentences/{id}");
    let us = sentence_variant("en-US", "zh-Hans", "It is in the center.");
    let save = json!({"expected_revision":1,"target_locale":"en-US","medium_locale":"zh-Hans","variant":us});
    assert_eq!(
        sentence_client(&state, outsider, "PUT", &path, save.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, saved) = sentence_client(&state, actor, "PUT", &path, save.clone()).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["draft"]["variants"][0],
        created["draft"]["variants"][0]
    );
    assert_eq!(
        sentence_client(&state, actor, "PUT", &path, save).await.0,
        StatusCode::CONFLICT
    );
    let mut traditional = uk.clone();
    traditional["translations"] =
        sentence_variant("en-GB", "zh-Hant", "unused")["translations"].clone();
    let (status,saved)=sentence_client(&state,actor,"PUT",&path,json!({"expected_revision":2,"target_locale":"en-GB","medium_locale":"zh-Hant","variant":traditional})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["draft"]["variants"][1], us);
    assert_eq!(
        saved["draft"]["variants"][0]["translations"][1],
        uk["translations"][0]
    );
    let (status,list)=sentence_client(&state,reader,"GET","/sentences?target_language=en&target_locale=en-GB&medium_locale=zh-Hant&keyword=centre&page=1&page_size=1",Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["translation_count"], 1);
    let mut bad = uk.clone();
    bad["locale"] = json!("en-US");
    assert_eq!(sentence_client(&state,actor,"PUT",&path,json!({"expected_revision":3,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":bad})).await.0,StatusCode::UNPROCESSABLE_ENTITY);
    let mut tx = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261115120000_lexicon_next_sentences.down.sql"
        ))
        .execute(&mut *tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        sentence_client(&state, reader, "GET", &path, Value::Null)
            .await
            .1,
        saved
    );
}
#[sqlx::test]
async fn next_sentence_references_preserve_old_clients_and_reject_foreign_ids(pool: PgPool) {
    let actor = admin(&pool, SENTENCE_EDIT).await;
    let state = AppState::for_test(pool.clone());
    let sentence = Uuid::now_v7();
    let (status,result)=sentence_client(&state,actor,"POST","/sentences",json!({"id":sentence,"target_language":"en","target_locale":"en-GB","medium_locale":"zh-Hans","variant":sentence_variant("en-GB","zh-Hans","The centre is here.")})).await;
    assert_eq!(status, StatusCode::CREATED, "{result}");
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let mut content = draft();
    content["senses"][0]["sentence_ids"] = json!([sentence]);
    let (status, saved) = sentence_client(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["draft"]["senses"][0]["sentence_ids"],
        json!([sentence])
    );
    let old = call(&state, Some(actor), "GET", &path, Value::Null).await.1;
    assert!(old["draft"]["senses"][0].get("sentence_ids").is_none());
    let (status, old_saved) = call(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":old["draft"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{old_saved}");
    let current = sentence_client(&state, actor, "GET", &path, Value::Null)
        .await
        .1;
    assert_eq!(
        current["draft"]["senses"][0]["sentence_ids"],
        json!([sentence])
    );
    assert_eq!(
        sentence_client(
            &state,
            actor,
            "GET",
            &format!("/sentences/{sentence}"),
            Value::Null
        )
        .await
        .1["reference_count"],
        1
    );
    let mut invalid = current["draft"].clone();
    invalid["senses"][0]["sentence_ids"] = json!([entry["id"]]);
    assert_eq!(
        sentence_client(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":invalid})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut missing = current["draft"].clone();
    missing["senses"][0]
        .as_object_mut()
        .unwrap()
        .remove("sentence_ids");
    assert_eq!(
        sentence_client(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":missing})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut removed = current["draft"].clone();
    removed["senses"][0]["sentence_ids"] = json!([]);
    let (status, saved) = sentence_client(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":removed}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        sentence_client(
            &state,
            actor,
            "GET",
            &format!("/sentences/{sentence}"),
            Value::Null
        )
        .await
        .1["reference_count"],
        0
    );
}

#[sqlx::test]
async fn next_sentence_audio_is_canonical_exclusive_readable_and_protected_from_reclamation(
    pool: PgPool,
) {
    use std::time::Duration;
    use tsz_rust::platform::storage::{
        MemoryAdapter, StoragePolicy, StoragePrivacy, StorageRegistry, StorageSpace,
    };
    let actor = admin(&pool, SENTENCE_EDIT).await;
    let reader = admin(&pool, &["sentences.access"]).await;
    let mut state = AppState::for_test(pool.clone());
    let storage = MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    );
    state.object_storage = StorageRegistry::from_stores([storage.clone()]).unwrap();
    let asset = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id,created_at) VALUES($1,$2,$3,'audio/mpeg',3,'en-GB','female','fixture.mp3',$4,now()-interval '100 days')")
        .bind(asset).bind(format!("local-fixture/{asset}")).bind(format!("local-fixture/source/{asset}")).bind(actor).execute(&pool).await.unwrap();
    let metadata:Value=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'locale',locale,'gender',gender,'content_type',content_type,'size_bytes',size_bytes,'duration_ms',duration_ms,'original_name',original_name,'created_at',created_at,'is_default',true) FROM lexicon.audio_assets WHERE id=$1").bind(asset).fetch_one(&pool).await.unwrap();
    let mut variant = sentence_variant("en-GB", "zh-Hans", "Listen here.");
    let mut forged = metadata.clone();
    forged["original_name"] = json!("forged.mp3");
    variant["audio_assets"] = json!([forged]);
    let id = Uuid::now_v7();
    let (status,created)=sentence_client(&state,actor,"POST","/sentences",json!({"id":id,"target_language":"en","target_locale":"en-GB","medium_locale":"zh-Hans","variant":variant})).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        created["draft"]["variants"][0]["audio_assets"][0]["original_name"],
        "fixture.mp3"
    );
    assert_eq!(
        tsz_rust::lexicon::audio_assets::reclaim_once(&pool, &storage)
            .await
            .unwrap(),
        0
    );
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/admin/lexicon/audio-assets/{asset}/url"))
                .header(
                    "authorization",
                    format!(
                        "Bearer {}",
                        state.admin_token_manager.generate(reader, "admin").unwrap()
                    ),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut other = variant.clone();
    other["id"] = json!(Uuid::now_v7());
    other["translations"] = json!([]);
    let (status,_)=sentence_client(&state,actor,"POST","/sentences",json!({"id":Uuid::now_v7(),"target_language":"en","target_locale":"en-GB","medium_locale":"zh-Hans","variant":other})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut wrong = variant.clone();
    wrong["locale"] = json!("en-US");
    wrong["id"] = json!(Uuid::now_v7());
    wrong["translations"] = json!([]);
    let (status,_)=sentence_client(&state,actor,"PUT",&format!("/sentences/{id}"),json!({"expected_revision":1,"target_locale":"en-US","medium_locale":"zh-Hans","variant":wrong})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    variant["audio_assets"] = json!([]);
    let (status,saved)=sentence_client(&state,actor,"PUT",&format!("/sentences/{id}"),json!({"expected_revision":1,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":variant})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        tsz_rust::lexicon::audio_assets::reclaim_once(&pool, &storage)
            .await
            .unwrap(),
        1
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.audio_assets WHERE id=$1")
        .bind(asset)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn next_sentence_new_sense_reference_requires_access_but_keep_and_remove_do_not(
    pool: PgPool,
) {
    let actor = admin(&pool, SENTENCE_EDIT).await;
    let state = AppState::for_test(pool.clone());
    let sentence = Uuid::now_v7();
    assert_eq!(sentence_client(&state,actor,"POST","/sentences",json!({"id":sentence,"target_language":"en","target_locale":"en-GB","medium_locale":"zh-Hans","variant":sentence_variant("en-GB","zh-Hans","Shared example.")})).await.0,StatusCode::CREATED);
    let entry = create(&state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let mut content = draft();
    content["senses"][0]["sentence_ids"] = json!([sentence]);
    content["senses"].as_array_mut().unwrap().push(json!({"id":Uuid::now_v7(),"part_of_speech":"noun","definitions":[],"equivalents":[],"sentence_ids":[]}));
    let (status, saved) = sentence_client(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='sentences.access'").bind(actor).execute(&pool).await.unwrap();
    let (status, kept) = sentence_client(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":saved["draft"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    let mut added = kept["draft"].clone();
    added["senses"][1]["sentence_ids"] = json!([sentence]);
    let (status, denied) = sentence_client(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":added}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    let current = sentence_client(&state, actor, "GET", &path, Value::Null)
        .await
        .1;
    assert_eq!(current["revision"], 3);
    assert_eq!(current["draft"]["senses"][1]["sentence_ids"], json!([]));
    let mut removed = kept["draft"].clone();
    removed["senses"][0]["sentence_ids"] = json!([]);
    let (status, saved) = sentence_client(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":removed}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let refs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon_next.sentence_references WHERE sentence_id=$1",
    )
    .bind(sentence)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(refs, 0);
}

#[sqlx::test]
async fn unified_sentence_drafts_are_atomic_scoped_and_permission_checked(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool, SENTENCE_EDIT).await;
    let outsider = admin(
        &pool,
        &[
            "words.access",
            "words.edit",
            "words.edit_others",
            "sentences.access",
            "sentences.edit",
        ],
    )
    .await;
    let entry = create(&state, actor).await;
    let word_path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let mut ids = [Uuid::now_v7(), Uuid::now_v7()];
    ids.sort();
    let [a, b] = ids;
    let mut word = draft();
    word["senses"][0]["sentence_ids"] = json!([a, b]);
    let uk = sentence_variant("en-GB", "zh-Hans", "A centre.");
    let us = sentence_variant("en-US", "zh-Hans", "A center.");
    let mut bad = sentence_variant("en-GB", "zh-Hans", "Another example.");
    bad["metadata"] = json!({"register_ids":[Uuid::now_v7()],"domain_ids":[],"sources":[]});
    let mut changes = json!([
        {"id":a,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":uk},
        {"id":a,"target_locale":"en-US","medium_locale":"zh-Hans","variant":us},
        {"id":b,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":bad}
    ]);
    let payload = json!({"expected_revision":1,"draft":word,"sentence_changes":changes});
    let (status, detail) = unified_sentence_client(&state, actor, "PUT", &word_path, payload).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{detail}");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM lexicon_next.sentences WHERE id=ANY($1)"
        )
        .bind(ids.to_vec())
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        call(&state, Some(actor), "GET", &word_path, Value::Null)
            .await
            .1["revision"],
        1
    );
    changes[2]["variant"]["metadata"]["register_ids"] = json!([]);
    let (status, saved) = unified_sentence_client(
        &state,
        actor,
        "PUT",
        &word_path,
        json!({"expected_revision":1,"draft":word,"sentence_changes":changes}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["revision"], 2);
    assert_eq!(saved["sentence_resources"].as_array().unwrap().len(), 2);
    let original = unified_sentence_client(&state, actor, "GET", &word_path, Value::Null)
        .await
        .1;
    assert_eq!(original, saved);
    assert!(
        call(&state, Some(actor), "GET", &word_path, Value::Null)
            .await
            .1
            .get("sentence_resources")
            .is_none()
    );
    let resources = saved["sentence_resources"].as_array().unwrap();
    let first = resources.iter().find(|r| r["id"] == a.to_string()).unwrap();
    assert_eq!(first["revision"], 1);
    assert_eq!(first["draft"]["variants"].as_array().unwrap().len(), 2);
    for change in changes.as_array_mut().unwrap() {
        change["expected_revision"] = json!(1);
    }
    changes[0]["variant"]["content"]["text"] = json!("Changed British.");
    changes[1]["variant"]["content"]["text"] = json!("Changed American.");
    changes[2]["expected_revision"] = json!(99);
    let request = json!({"expected_revision":2,"draft":word,"sentence_changes":changes});
    assert_eq!(
        unified_sentence_client(&state, actor, "PUT", &word_path, request.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        unified_sentence_client(&state, actor, "GET", &word_path, Value::Null)
            .await
            .1,
        original
    );
    changes[2]["expected_revision"] = json!(1);
    let valid = json!({"expected_revision":2,"draft":word,"sentence_changes":changes});
    assert_eq!(
        unified_sentence_client(&state, outsider, "PUT", &word_path, valid.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        unified_sentence_client(&state, actor, "GET", &word_path, Value::Null)
            .await
            .1,
        original
    );
    // Other medium translations survive the coordinated update.
    let mut traditional = first["draft"]["variants"][0].clone();
    traditional["translations"] =
        sentence_variant("en-GB", "zh-Hant", "unused")["translations"].clone();
    let path = format!("/sentences/{a}");
    let (status, enriched) = sentence_client_details(&state,actor,"PUT",&path,json!({"expected_revision":1,"target_locale":"en-GB","medium_locale":"zh-Hant","variant":traditional}),true).await;
    assert_eq!(status, StatusCode::OK, "{enriched}");
    changes[0]["expected_revision"] = json!(2);
    changes[1]["expected_revision"] = json!(2);
    let (status, saved) = unified_sentence_client(
        &state,
        actor,
        "PUT",
        &word_path,
        json!({"expected_revision":2,"draft":word,"sentence_changes":changes}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let first = saved["sentence_resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == a.to_string())
        .unwrap();
    assert_eq!(first["revision"], 3);
    let uk = first["draft"]["variants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["locale"] == "en-GB")
        .unwrap();
    assert_eq!(uk["content"]["text"], "Changed British.");
    assert!(
        uk["translations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["locale"] == "zh-Hant")
    );
}

#[sqlx::test]
async fn next_sentence_association_roundtrips_to_a_next_entry_target(pool: PgPool) {
    let actor = admin(
        &pool,
        &[
            "words.access",
            "words.create",
            "words.edit",
            "sentences.access",
            "sentences.create",
            "sentences.edit",
        ],
    )
    .await;
    let state = AppState::for_test(pool.clone());
    let target_entry = Uuid::now_v7();
    let target_pos = Uuid::now_v7();
    let target_form = Uuid::now_v7();
    let target_variant = Uuid::now_v7();
    let target_sense = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon_next.entries(id,target_language,medium_locale,kind,headword,owner_id,revision) VALUES($1,'en','zh-Hans','word','target',$2,1)")
        .bind(target_entry)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO lexicon_next.draft_revisions(entry_id,revision,actor_id,document) VALUES($1,1,$2,$3)")
        .bind(target_entry)
        .bind(actor)
        .bind(json!({"schema_version":1,"headword":"target","forms":[],"senses":[],"form_editor":null}))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO lexicon_next.nodes(id,entry_id,kind,parent_id) VALUES($1,$2,'sense',NULL)",
    )
    .bind(target_sense)
    .bind(target_entry)
    .execute(&pool)
    .await
    .unwrap();
    for (id, role, parent) in [
        (target_pos, "pos", None),
        (target_form, "form", Some(target_pos)),
        (target_variant, "variant", Some(target_form)),
    ] {
        sqlx::query("INSERT INTO lexicon_next.form_editor_nodes(id,entry_id,role,parent_id) VALUES($1,$2,$3,$4)")
            .bind(id)
            .bind(target_entry)
            .bind(role)
            .bind(parent)
            .execute(&pool)
            .await
            .unwrap();
    }
    let sentence = Uuid::now_v7();
    let variant = Uuid::now_v7();
    let body = json!({
        "id": sentence,
        "target_language": "en",
        "target_locale": "en-GB",
        "medium_locale": "zh-Hans",
        "variant": {
            "id": variant,
            "locale": "en-GB",
            "level": "A1",
            "content": {"version":2,"text":"target","annotations":[]},
            "translations": [],
            "associations": [{
                "id": Uuid::now_v7(),
                "source_dialect": "uk",
                "source_segments": [{"start":0,"end":6,"surface":"target"}],
                "target": {
                    "state":"linked",
                    "target_source":"lexicon_next",
                    "target_entry_id": target_entry,
                    "target_pos_id": target_pos,
                    "target_base_form_id": target_form,
                    "target_form_id": target_form,
                    "target_variant_id": target_variant,
                    "target_sense_id": target_sense,
                    "target_dialect":"common"
                }
            }]
        }
    });
    sqlx::query("UPDATE lexicon_next.entries SET kind='phrase' WHERE id=$1")
        .bind(target_entry)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call_associations(&state, actor, "POST", "/sentences", body.clone())
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    sqlx::query("UPDATE lexicon_next.entries SET kind='word' WHERE id=$1")
        .bind(target_entry)
        .execute(&pool)
        .await
        .unwrap();
    let mut invalid = body.clone();
    invalid["variant"]["associations"][0]["source_segments"][0]["end"] = json!(99);
    assert_eq!(
        call_associations(&state, actor, "POST", "/sentences", invalid)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (status, saved) = call_associations(&state, actor, "POST", "/sentences", body).await;
    assert_eq!(status, StatusCode::CREATED, "{saved}");
    assert_eq!(
        saved["draft"]["variants"][0]["associations"][0]["target"]["target_source"],
        "lexicon_next"
    );
    let (status, read) = call_associations(
        &state,
        actor,
        "GET",
        &format!("/sentences/{sentence}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(
        read["draft"]["variants"][0]["associations"][0]["target"]["target_entry_id"],
        target_entry.to_string()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon_next.sentence_associations WHERE sentence_id=$1 AND target_source='lexicon_next'")
        .bind(sentence)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[path = "lexicon_next/relations.rs"]
mod relations;

#[path = "lexicon_next/publications.rs"]
mod publications;
