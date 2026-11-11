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
    let mut req = Request::builder()
        .method(method)
        .uri(format!("{ROOT}{path}"))
        .header("content-type", "application/json");
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
