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
    let (groups, ownership, grammar) = capabilities;
    let mut req = Request::builder()
        .method(method)
        .uri(format!("{ROOT}{path}"))
        .header("content-type", "application/json")
        .header("x-tsz-next-forms", "v1");
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
async fn creation_matches_current_forms_empty_drafts_and_archived_entries(pool: PgPool) {
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
    assert_eq!(first["pagination"]["total"], 3);
    assert_eq!(first["pagination"]["total_pages"], 2);
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert!(
        first["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["status"] == "draft")
    );
    let mut second_query = query.clone();
    second_query["page"] = json!(2);
    let (status, second) = call(&state, Some(actor), "POST", "/entry-matches", second_query).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["pagination"]["page"], 2);
    assert_eq!(second["items"][0]["status"], "archived");
    let mut beyond_query = query.clone();
    beyond_query["page"] = json!(99);
    let (status, beyond) = call(&state, Some(actor), "POST", "/entry-matches", beyond_query).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(beyond["pagination"]["page"], 2);
    assert_eq!(beyond["items"], second["items"]);
    let ids = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["items"].as_array().unwrap())
        .map(|row| row["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        ids,
        std::collections::BTreeSet::from([
            entries[0]["id"].as_str().unwrap(),
            entries[1]["id"].as_str().unwrap(),
            entries[4]["id"].as_str().unwrap()
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
async fn homonym_annotations_use_forms_or_initial_headword_fallback(pool: PgPool) {
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
    let first = create(&state, actor).await;
    let mut content = draft();
    content["forms"] = json!([{ "id":Uuid::now_v7(), "locale":"en-US", "text":"center" }]);
    let (status, first) = call(
        &state,
        Some(actor),
        "PUT",
        &format!("/entries/{}", first["id"].as_str().unwrap()),
        json!({"expected_revision":1,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, second) = call(
        &state,
        Some(actor),
        "POST",
        "/entries",
        json!({"target_language":"en","kind":"word","headword":"center"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let initial_only = create(&state, actor).await;
    let (_, list) = call(
        &state,
        Some(actor),
        "GET",
        "/workspace/entries",
        Value::Null,
    )
    .await;
    for (entry, visible) in [(&first, true), (&second, true), (&initial_only, false)] {
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
    for (entry, annotation, expected) in [
        (&first, "1", StatusCode::OK),
        (&second, "1", StatusCode::CONFLICT),
        (&second, "2", StatusCode::OK),
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
    let path = format!("/entries/{}", second["id"].as_str().unwrap());
    let mut second_draft = second["draft"].clone();
    second_draft["forms"] = json!([{ "id":Uuid::now_v7(), "locale":"en-GB", "text":"center" }]);
    for (revision, locale, visible) in [(1, "en-GB", false), (2, "en", true)] {
        second_draft["forms"][0]["locale"] = json!(locale);
        let (status, saved) = call(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":revision,"draft":second_draft}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        let (_, list) = call(
            &state,
            Some(actor),
            "GET",
            "/workspace/entries",
            Value::Null,
        )
        .await;
        for row in list["items"].as_array().unwrap() {
            assert_eq!(
                row["annotation_visible"],
                visible && row["id"] != initial_only["id"],
                "{row}"
            );
        }
    }
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
            .all(|row| row["annotation_visible"] == false)
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
    assert_eq!(caps["publishing"], false);
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
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261114130000_lexicon_next_grammar.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
}
