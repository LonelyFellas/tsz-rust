use super::*;

async fn fixture(state: &AppState, pool: &PgPool, actor: Uuid) -> (String, Value) {
    let entry = create(state, actor).await;
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let mut content = draft();
    content["form_editor"] = form_editor();
    let group = Uuid::now_v7();
    content["sense_groups"] = json!([{"id":group,"name_zh":"位置","name_en":"position"}]);
    content["grammar_structures"] = json!([]);
    let sub: String = sqlx::query_scalar("SELECT s.code FROM catalog.sub_parts_of_speech s JOIN catalog.parts_of_speech p ON p.id=s.part_of_speech_id WHERE p.code='noun' ORDER BY s.code LIMIT 1").fetch_one(pool).await.unwrap();
    for sense in content["senses"].as_array_mut().unwrap() {
        sense["level"] = json!("A1");
        sense["sub_pos"] = json!(sub);
        sense["sense_group_id"] = json!(group);
        for definition in sense["definitions"].as_array_mut().unwrap() {
            definition["level"] = json!("A1");
            definition["grammar_unbound"] = json!(true);
        }
    }
    // Raw complete wire uses all authoring capabilities, as does Admin.
    let (status, saved) = full_call(
        state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    (path, saved)
}
async fn full_call(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let pairs = !path.ends_with("?legacy");
    let mut req = Request::builder()
        .method(method)
        .uri(format!("{ROOT}{path}"))
        .header("content-type", "application/json");
    for header in [
        "forms",
        "medium-ownership",
        "sense-groups",
        "grammar",
        "sense-metadata",
        "definitions",
        "sentences",
        "sentence-details",
        "sentence-drafts",
        "equivalents",
        "relations",
        "publications",
        "levels",
    ] {
        req = req.header(format!("x-tsz-next-{header}"), "v1");
    }
    if !path.ends_with("?legacy-questions") {
        req = req.header("x-tsz-next-form-questions", "v1");
    }
    if pairs {
        req = req.header("x-tsz-next-dialect-pairs", "v1");
    }
    req = req.header(
        "authorization",
        format!(
            "Bearer {}",
            state.admin_token_manager.generate(actor, "admin").unwrap()
        ),
    );
    let response = tsz_rust::router(state.clone())
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, value)
}
async fn checked(
    state: &AppState,
    actor: Uuid,
    path: &str,
    revision: i64,
    lifecycle: i64,
) -> Value {
    let (status, preview) = full_call(
        state,
        actor,
        "POST",
        &format!("{path}/publication-preview"),
        json!({"base_revision":revision,"base_lifecycle_revision":lifecycle}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    preview
}
fn command(preview: &Value) -> Value {
    json!({"request_id":Uuid::now_v7(),"base_revision":preview["revision"],"base_lifecycle_revision":preview["lifecycle_revision"],"preview_token":preview["preview_token"]})
}
const PUBLISH: &[&str] = &[
    "words.access",
    "words.create",
    "words.edit",
    "words.publish",
    "words.archive",
    "words.restore",
];

async fn assert_form_type_in_use(state: &AppState, actor: Uuid, form_type: Uuid) {
    let request = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/admin/settings/form-types/{form_type}?base_revision=1"
        ))
        .header(
            "authorization",
            format!(
                "Bearer {}",
                state.admin_token_manager.generate(actor, "admin").unwrap()
            ),
        )
        .body(Body::empty())
        .unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "form_type_in_use");
    assert_eq!(body["meta"]["usage_count"], 1);
}

#[sqlx::test]
async fn phrase_form_groups_round_trip_and_publish(pool: PgPool) {
    let pos_id = Uuid::now_v7();
    let plural_id = Uuid::now_v7();
    sqlx::query("INSERT INTO catalog.parts_of_speech (id, kind, code, name_zh, name_en, short_name_zh, abbreviation, full_name_en) VALUES ($1, 'phrase', 'phrase_noun', '名词性成分', 'Nominal constituent', '名词性成分', 'np.', 'nominal constituent')")
        .bind(pos_id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO catalog.form_types (id, part_of_speech_id, code, name_zh, name_en, short_name_zh, abbreviation, full_name_en) VALUES ($1, $2, 'phrase_noun_plural', '复数', 'Plural', '复数', 'pl.', 'plural')")
        .bind(plural_id).bind(pos_id).execute(&pool).await.unwrap();
    let mut permissions = PUBLISH.to_vec();
    permissions.extend(["lexicon_settings.access", "lexicon_settings.delete"]);
    let actor = admin(&pool, &permissions).await;
    let state = AppState::for_test(pool);
    let (status, entry) = full_call(
        &state,
        actor,
        "POST",
        "/entries",
        json!({"target_language":"en","kind":"phrase","headword":"city centre"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{entry}");
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());

    let mut editor = form_editor();
    editor["pos"][0]["pos"] = json!("phrase_noun");
    editor["contents"][0]["regional_variants"]["uk"]["spelling"] = json!("city centre");
    editor["contents"][0]["regional_variants"]["us"]["spelling"] = json!("city center");
    let mut plural = form_editor();
    plural["contents"][0]["regional_variants"]["uk"]["spelling"] = json!("city centres");
    plural["contents"][0]["regional_variants"]["us"]["spelling"] = json!("city centers");
    plural["pos"][0]["forms"][0]["form_type"] = json!("phrase_noun_plural");
    editor["contents"]
        .as_array_mut()
        .unwrap()
        .push(plural["contents"][0].take());
    for field in ["forms", "form_groups"] {
        editor["pos"][0][field]
            .as_array_mut()
            .unwrap()
            .push(plural["pos"][0][field][0].take());
    }

    let group = Uuid::now_v7();
    let content = json!({
        "schema_version":1,"headword":"city centre","forms":[],"form_editor":editor,
        "sense_groups":[{"id":group,"name_zh":"中心城区","name_en":"central district"}],
        "senses":[{"id":Uuid::now_v7(),"part_of_speech":"phrase_noun","level":"A1","sense_group_id":group,"equivalents":[],
            "definitions":[
                {"id":Uuid::now_v7(),"locale":"en","style":"definition","text":"the central part of a city","level":"A1","grammar_unbound":true},
                {"id":Uuid::now_v7(),"locale":"zh-Hans","style":"definition","text":"市中心","level":"A1","grammar_unbound":true}
            ]
        }]
    });
    let (status, saved) = full_call(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_form_type_in_use(&state, actor, plural_id).await;
    let (status, reopened) = full_call(&state, actor, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    assert_eq!(
        reopened["draft"]["form_editor"],
        saved["draft"]["form_editor"]
    );
    assert_eq!(
        reopened["draft"]["form_editor"]["pos"][0]["form_groups"],
        editor["pos"][0]["form_groups"]
    );
    let preview = checked(&state, actor, &path, 2, 1).await;
    assert_eq!(preview["can_publish"], true, "{preview}");
    let (status, published) = full_call(
        &state,
        actor,
        "POST",
        &format!("{path}/publications"),
        command(&preview),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_form_type_in_use(&state, actor, plural_id).await;
    let (status, read) = full_call(
        &state,
        actor,
        "GET",
        &format!("{path}/publication"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(read["status"], "published");
    assert_eq!(
        read["current"]["snapshot"]["draft"]["form_editor"],
        reopened["draft"]["form_editor"]
    );

    let mut updated = reopened["draft"].clone();
    let updated_editor = &mut updated["form_editor"];
    updated_editor["contents"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    for field in ["forms", "form_groups"] {
        updated_editor["pos"][0][field]
            .as_array_mut()
            .unwrap()
            .truncate(1);
    }
    let (status, saved) = full_call(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":updated}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_form_type_in_use(&state, actor, plural_id).await;
}

#[sqlx::test]
async fn publication_freezes_content_replays_and_preserves_history(pool: PgPool) {
    let actor = admin(&pool, PUBLISH).await;
    let single = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let state = AppState::for_test(single);
    let (path, saved) = fixture(&state, &pool, actor).await;
    let preview = checked(&state, actor, &path, 2, 1).await;
    assert_eq!(preview["can_publish"], true, "{preview}");
    assert!(
        !preview["warnings"].as_array().unwrap().is_empty(),
        "text publication must warn about missing audio"
    );
    let input = command(&preview);
    let url = format!("{path}/publications");
    let ((status, published), (replay_status, replay)) = tokio::join!(
        full_call(&state, actor, "POST", &url, input.clone()),
        full_call(&state, actor, "POST", &url, input.clone())
    );
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(replay_status, StatusCode::OK, "{replay}");
    assert_eq!(published, replay);
    assert_eq!(published["snapshot"]["draft"], saved["draft"]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM lexicon_next.publications")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let (_, read) = full_call(
        &state,
        actor,
        "GET",
        &format!("{path}/publication"),
        Value::Null,
    )
    .await;
    assert_eq!(read["status"], "published");
    assert_eq!(read["has_unpublished_changes"], false);
    assert_eq!(read["current"], published);
    let id = saved["id"].as_str().unwrap();
    let (_, old_row) = call(
        &state,
        Some(actor),
        "GET",
        &format!("/workspace/entries/{id}"),
        Value::Null,
    )
    .await;
    let (_, new_row) = full_call(
        &state,
        actor,
        "GET",
        &format!("/workspace/entries/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(
        old_row["status"], "draft",
        "old strict enum remains compatible"
    );
    assert_eq!(new_row["status"], "published");
    let (_, list) = full_call(
        &state,
        actor,
        "GET",
        "/workspace/entries?status=published",
        Value::Null,
    )
    .await;
    assert_eq!(list["items"][0]["id"], saved["id"]);
    let mut changed = saved["draft"].clone();
    changed["headword"] = json!("changed draft");
    assert_eq!(
        full_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":changed})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, read) = full_call(
        &state,
        actor,
        "GET",
        &format!("{path}/publication"),
        Value::Null,
    )
    .await;
    assert_eq!(read["has_unpublished_changes"], true);
    assert_eq!(
        read["current"], published,
        "later saves cannot mutate a publication"
    );
    let mut stale = input.clone();
    stale["request_id"] = json!(Uuid::now_v7());
    assert_eq!(
        full_call(&state, actor, "POST", &url, stale).await.0,
        StatusCode::CONFLICT
    );
    // Fresh authorization is required even when replaying a successful command.
    sqlx::query(
        "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='words.publish'",
    )
    .bind(actor)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        full_call(&state, actor, "POST", &url, input).await.0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn publication_rejects_incomplete_foreign_archived_and_stale_drafts(pool: PgPool) {
    let actor = admin(&pool, PUBLISH).await;
    let other = admin(&pool, PUBLISH).await;
    let editor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let initial = create(&state, actor).await;
    let path = format!("/entries/{}", initial["id"].as_str().unwrap());
    let preview = checked(&state, actor, &path, 1, 1).await;
    assert_eq!(preview["can_publish"], false);
    assert!(
        preview["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["field"] == "senses")
    );
    assert_eq!(
        full_call(
            &state,
            actor,
            "POST",
            &format!("{path}/publications"),
            command(&preview)
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for person in [other, editor] {
        assert_eq!(
            full_call(
                &state,
                person,
                "POST",
                &format!("{path}/publication-preview"),
                json!({"base_revision":1,"base_lifecycle_revision":1})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        full_call(
            &state,
            actor,
            "POST",
            &format!("{path}/publication-preview"),
            json!({"base_revision":2,"base_lifecycle_revision":1})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, archived) = call(&state, Some(actor), "POST", "/workspace/commands", json!({"request_id":Uuid::now_v7(),"action":"archive","items":[{"entry_id":initial["id"],"base_revision":1,"base_lifecycle_revision":1}]})).await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    assert_eq!(
        full_call(
            &state,
            actor,
            "POST",
            &format!("{path}/publication-preview"),
            json!({"base_revision":1,"base_lifecycle_revision":2})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM lexicon_next.publications")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn publication_binds_shared_sentence_versions_and_permissions(pool: PgPool) {
    let mut grants = PUBLISH.to_vec();
    grants.extend([
        "sentences.access",
        "sentences.create",
        "sentences.edit",
        "sentences.publish",
    ]);
    let actor = admin(&pool, &grants).await;
    let state = AppState::for_test(pool.clone());
    let (path, saved) = fixture(&state, &pool, actor).await;
    let domain = equivalent_taxonomy(&pool, "domains", "published").await;
    let sentence = Uuid::now_v7();
    let variant = json!({"id":Uuid::now_v7(),"locale":"en-GB","level":"A1","content":{"version":2,"text":"Meet me in the centre.","annotations":[]},"translations":[],"metadata":{"register_ids":[],"domain_ids":[domain],"sources":[]}});
    let (status, created) = full_call(&state, actor, "POST", "/sentences", json!({"id":sentence,"target_language":"en","target_locale":"en-GB","medium_locale":"zh-Hans","variant":variant})).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let mut content = saved["draft"].clone();
    content["senses"][0]["sentence_ids"] = json!([sentence]);
    assert_eq!(
        full_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":content})
        )
        .await
        .0,
        StatusCode::OK
    );
    let old_preview = checked(&state, actor, &path, 3, 1).await;
    assert_eq!(old_preview["can_publish"], true, "{old_preview}");
    let mut changed = variant.clone();
    changed["content"]["text"] = json!("The centre is nearby.");
    assert_eq!(full_call(&state, actor, "PUT", &format!("/sentences/{sentence}"), json!({"expected_revision":1,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":changed})).await.0, StatusCode::OK);
    assert_eq!(
        full_call(
            &state,
            actor,
            "POST",
            &format!("{path}/publications"),
            command(&old_preview)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let preview = checked(&state, actor, &path, 3, 1).await;
    let (status, published) = full_call(
        &state,
        actor,
        "POST",
        &format!("{path}/publications"),
        command(&preview),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(
        published["snapshot"]["sentence_resources"][0]["revision"],
        2
    );
    let mut without_metadata = variant.clone();
    without_metadata["metadata"] = json!({"register_ids":[],"domain_ids":[],"sources":[]});
    assert_eq!(full_call(&state, actor, "PUT", &format!("/sentences/{sentence}"), json!({"expected_revision":2,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":without_metadata})).await.0, StatusCode::OK);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM lexicon_next.sentence_taxonomy_refs WHERE taxonomy_id=$1"
        )
        .bind(domain)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    let (_, read) = full_call(
        &state,
        actor,
        "GET",
        &format!("{path}/publication"),
        Value::Null,
    )
    .await;
    assert_eq!(read["current"], published);
    assert_eq!(read["has_unpublished_changes"], true);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM lexicon_next.publication_taxonomy_refs WHERE taxonomy_id=$1"
        )
        .bind(domain)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert!(
        sqlx::query("DELETE FROM catalog.lexicon_taxonomies WHERE id=$1")
            .bind(domain)
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='sentences.publish'").bind(actor).execute(&pool).await.unwrap();
    assert_eq!(
        full_call(
            &state,
            actor,
            "POST",
            &format!("{path}/publication-preview"),
            json!({"base_revision":3,"base_lifecycle_revision":2})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn published_audio_survives_draft_removal_and_reclamation(pool: PgPool) {
    use std::{sync::Arc, time::Duration};
    use tsz_rust::platform::storage::{
        MemoryAdapter, ObjectStore, StoragePolicy, StoragePrivacy, StorageSpace,
    };
    let actor = admin(&pool, PUBLISH).await;
    let state = AppState::for_test(pool.clone());
    let (path, saved) = fixture(&state, &pool, actor).await;
    let asset = Uuid::now_v7();
    let key = format!("tshb-test-assets/temporary-tests/next-publish-20261116/audio/{asset}.mp3");
    sqlx::query("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id,created_at) VALUES($1,$2,$2,'audio/mpeg',3,'en-GB','female','fixture.mp3',$3,now()-interval '100 days')").bind(asset).bind(key).bind(actor).execute(&pool).await.unwrap();
    let mut content = saved["draft"].clone();
    content["senses"][0]["definitions"][0]["content"] =
        json!({"version":2,"text":content["senses"][0]["definitions"][0]["text"],"annotations":[]});
    content["senses"][0]["definitions"][0]["audio_assets"] = json!([{"id":asset,"locale":"en-GB","gender":"female","content_type":"audio/mpeg","size_bytes":3,"duration_ms":null,"original_name":"fixture.mp3","created_at":"2026-11-01T00:00:00Z"}]);
    let (status, with_audio) = full_call(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{with_audio}");
    let preview = checked(&state, actor, &path, 3, 1).await;
    assert_eq!(preview["can_publish"], true, "{preview}");
    let (status, published) = full_call(
        &state,
        actor,
        "POST",
        &format!("{path}/publications"),
        command(&preview),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    let mut removed = with_audio["draft"].clone();
    removed["senses"][0]["definitions"][0]
        .as_object_mut()
        .unwrap()
        .remove("audio_assets");
    assert_eq!(
        full_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":3,"draft":removed})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM lexicon_next.audio_asset_references WHERE asset_id=$1"
        )
        .bind(asset)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM lexicon_next.publication_audio_references WHERE asset_id=$1"
        )
        .bind(asset)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    let storage: Arc<dyn ObjectStore> = MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(
            StoragePrivacy::Private,
            1024 * 1024,
            Duration::from_secs(60),
            None,
        )
        .unwrap(),
    );
    assert_eq!(
        tsz_rust::lexicon::audio_assets::reclaim_once(&pool, &storage)
            .await
            .unwrap(),
        0
    );
    assert!(sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM lexicon.audio_assets WHERE id=$1 AND reclamation_started_at IS NULL)").bind(asset).fetch_one(&pool).await.unwrap());
    // Publication history is a hard retention boundary, including migration rollback.
    assert!(
        sqlx::raw_sql(include_str!(
            "../../migrations/20261116090000_lexicon_next_publications.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
}

#[sqlx::test]
async fn publication_migration_round_trip_preserves_existing_drafts(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let initial = create(&state, actor).await;
    sqlx::raw_sql(include_str!(
        "../../migrations/20261126020000_assessed_learning_sources.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261116090000_lexicon_next_publications.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261116090000_lexicon_next_publications.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261126020000_assessed_learning_sources.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let (_, read) = call(
        &state,
        Some(actor),
        "GET",
        &format!("/entries/{}", initial["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(read, initial);
}

#[sqlx::test]
async fn publication_pins_relation_versions_and_blocks_archiving_live_targets(pool: PgPool) {
    let actor = admin(&pool, PUBLISH).await;
    let state = AppState::for_test(pool.clone());
    let (target_path, target) = fixture(&state, &pool, actor).await;
    let preview = checked(&state, actor, &target_path, 2, 1).await;
    assert_eq!(
        full_call(
            &state,
            actor,
            "POST",
            &format!("{target_path}/publications"),
            command(&preview)
        )
        .await
        .0,
        StatusCode::OK
    );
    let (source_path, source) = fixture(&state, &pool, actor).await;
    let mut content = source["draft"].clone();
    content["senses"][0]["relations"] = json!([{"id":Uuid::now_v7(),"relation":"synonym","target_word_id":target["id"],"target_sense_id":target["draft"]["senses"][0]["id"],"score":"80"}]);
    assert_eq!(
        full_call(
            &state,
            actor,
            "PUT",
            &source_path,
            json!({"expected_revision":2,"draft":content})
        )
        .await
        .0,
        StatusCode::OK
    );
    let old_preview = checked(&state, actor, &source_path, 3, 1).await;
    let mut changed = target["draft"].clone();
    changed["headword"] = json!("new target");
    assert_eq!(
        full_call(
            &state,
            actor,
            "PUT",
            &target_path,
            json!({"expected_revision":2,"draft":changed})
        )
        .await
        .0,
        StatusCode::OK
    );
    let preview = checked(&state, actor, &target_path, 3, 2).await;
    let (status, target_published) = full_call(
        &state,
        actor,
        "POST",
        &format!("{target_path}/publications"),
        command(&preview),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{target_published}");
    assert_eq!(
        full_call(
            &state,
            actor,
            "POST",
            &format!("{source_path}/publications"),
            command(&old_preview)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let preview = checked(&state, actor, &source_path, 3, 1).await;
    let (status, published) = full_call(
        &state,
        actor,
        "POST",
        &format!("{source_path}/publications"),
        command(&preview),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(
        published["snapshot"]["relation_targets"][0]["publication_id"],
        target_published["id"]
    );
    assert_eq!(
        published["snapshot"]["relation_targets"][0]["headword"],
        "new target"
    );
    assert!(
        published["snapshot"]["relation_targets"][0]["forms"]
            .as_array()
            .unwrap()
            .iter()
            .any(|form| form["locale"] == "en-US" && form["text"] == "center")
    );
    content["senses"][0]["relations"] = json!([]);
    assert_eq!(
        full_call(
            &state,
            actor,
            "PUT",
            &source_path,
            json!({"expected_revision":3,"draft":content})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, error) = full_call(&state, actor, "POST", "/workspace/commands", json!({"request_id":Uuid::now_v7(),"action":"archive","items":[{"entry_id":target["id"],"base_revision":3,"base_lifecycle_revision":3}]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(
        error["detail"]
            .as_str()
            .unwrap()
            .contains("已发布拓展词引用")
    );
}

#[sqlx::test]
async fn publication_archive_restore_and_delete_preserve_frozen_history(pool: PgPool) {
    let actor = admin(&pool, PUBLISH).await;
    sqlx::query("UPDATE admins SET role='super_admin' WHERE id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let state = AppState::for_test(pool.clone());
    let (path, saved) = fixture(&state, &pool, actor).await;
    let preview = checked(&state, actor, &path, 2, 1).await;
    let (_, published) = full_call(
        &state,
        actor,
        "POST",
        &format!("{path}/publications"),
        command(&preview),
    )
    .await;
    assert!(
        sqlx::query("UPDATE lexicon_next.publications SET snapshot=snapshot WHERE entry_id=$1")
            .bind(saved["id"].as_str().unwrap().parse::<Uuid>().unwrap())
            .execute(&pool)
            .await
            .is_err()
    );
    for (action, lifecycle, expected) in [
        ("archive", 2, "archived"),
        ("restore", 3, "published"),
        ("archive", 4, "archived"),
    ] {
        let (status, result) = full_call(&state, actor, "POST", "/workspace/commands", json!({"request_id":Uuid::now_v7(),"action":action,"items":[{"entry_id":saved["id"],"base_revision":2,"base_lifecycle_revision":lifecycle}]})).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        let (_, read) = full_call(
            &state,
            actor,
            "GET",
            &format!("{path}/publication"),
            Value::Null,
        )
        .await;
        assert_eq!(read["status"], expected);
        assert_eq!(read["current"], published);
    }
    let (status, error) = full_call(&state, actor, "POST", "/workspace/commands", json!({"request_id":Uuid::now_v7(),"action":"delete","items":[{"entry_id":saved["id"],"base_revision":2,"base_lifecycle_revision":5}]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(error["detail"].as_str().unwrap().contains("发布历史"));
}

#[sqlx::test]
async fn publication_restore_requires_available_frozen_relation_targets(pool: PgPool) {
    let actor = admin(&pool, PUBLISH).await;
    let state = AppState::for_test(pool.clone());
    let (target_path, target) = fixture(&state, &pool, actor).await;
    let preview = checked(&state, actor, &target_path, 2, 1).await;
    assert_eq!(
        full_call(
            &state,
            actor,
            "POST",
            &format!("{target_path}/publications"),
            command(&preview)
        )
        .await
        .0,
        StatusCode::OK
    );
    let (source_path, source) = fixture(&state, &pool, actor).await;
    let mut content = source["draft"].clone();
    content["senses"][0]["relations"] = json!([{"id":Uuid::now_v7(),"relation":"synonym","target_word_id":target["id"],"target_sense_id":target["draft"]["senses"][0]["id"],"score":"80"}]);
    assert_eq!(
        full_call(
            &state,
            actor,
            "PUT",
            &source_path,
            json!({"expected_revision":2,"draft":content})
        )
        .await
        .0,
        StatusCode::OK
    );
    let preview = checked(&state, actor, &source_path, 3, 1).await;
    assert_eq!(
        full_call(
            &state,
            actor,
            "POST",
            &format!("{source_path}/publications"),
            command(&preview)
        )
        .await
        .0,
        StatusCode::OK
    );
    content["senses"][0]["relations"] = json!([]);
    assert_eq!(
        full_call(
            &state,
            actor,
            "PUT",
            &source_path,
            json!({"expected_revision":3,"draft":content})
        )
        .await
        .0,
        StatusCode::OK
    );
    for (id, revision) in [(&source["id"], 4), (&target["id"], 2)] {
        let (status, result) = full_call(&state, actor, "POST", "/workspace/commands", json!({"request_id":Uuid::now_v7(),"action":"archive","items":[{"entry_id":id,"base_revision":revision,"base_lifecycle_revision":2}]})).await;
        assert_eq!(status, StatusCode::OK, "{result}");
    }
    let restore = json!({"request_id":Uuid::now_v7(),"action":"restore","items":[{"entry_id":source["id"],"base_revision":4,"base_lifecycle_revision":3}]});
    let (status, result) = full_call(&state, actor, "POST", "/workspace/commands", restore).await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    let (_, read) = full_call(
        &state,
        actor,
        "GET",
        &format!("{source_path}/publication"),
        Value::Null,
    )
    .await;
    assert_eq!(read["status"], "archived");
    assert_eq!(read["lifecycle_revision"], 3);
    let restore_both = json!({"request_id":Uuid::now_v7(),"action":"restore","items":[{"entry_id":source["id"],"base_revision":4,"base_lifecycle_revision":3},{"entry_id":target["id"],"base_revision":2,"base_lifecycle_revision":3}]});
    let (status, result) =
        full_call(&state, actor, "POST", "/workspace/commands", restore_both).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let (_, read) = full_call(
        &state,
        actor,
        "GET",
        &format!("{source_path}/publication"),
        Value::Null,
    )
    .await;
    assert_eq!(read["status"], "published");
}

#[sqlx::test]
async fn phrase_can_save_publish_and_reload_with_catalog_grammar(pool: PgPool) {
    let actor = admin(&pool, PUBLISH).await;
    let state = AppState::for_test(pool.clone());
    let (status, entry) = full_call(
        &state,
        actor,
        "POST",
        "/entries",
        json!({
            "target_language":"en","medium_locale":"zh-Hans","kind":"phrase","headword":"take off"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{entry}");
    let path = format!("/entries/{}", entry["id"].as_str().unwrap());
    let part = Uuid::now_v7();
    sqlx::query("INSERT INTO catalog.parts_of_speech(id,code,kind,name_zh,name_en,abbreviation,short_name_zh,full_name_en) VALUES($1,'phrase_verbal_constituent','phrase','动词性成分','Verbal constituent','V','动词性成分','Verbal constituent')").bind(part).execute(&pool).await.unwrap();
    let sub = "PHRASE_INTRANSITIVE";
    sqlx::query("INSERT INTO catalog.sub_parts_of_speech(id,part_of_speech_id,code,name_zh,name_en,short_name_zh,abbreviation,full_name_en) VALUES($1,$2,$3,'不及物动副短语','Intransitive verbal phrase','不及物短语','V-I','Intransitive verbal phrase')").bind(Uuid::now_v7()).bind(part).bind(sub).execute(&pool).await.unwrap();
    let group = Uuid::now_v7();
    let grammar = Uuid::now_v7();
    let mut editor = form_editor();
    editor["pos"][0]["pos"] = json!("phrase_verbal_constituent");
    for region in ["uk", "us"] {
        editor["contents"][0]["regional_variants"][region]["spelling"] = json!("take off");
    }
    let draft = json!({"schema_version":1,"headword":"take off","forms":[],"form_editor":editor,
        "sense_groups":[{"id":group,"name_zh":"起飞","name_en":"flight"}],
        "grammar_structures":[{"id":grammar,"part_of_speech":"phrase_verbal_constituent","sub_pos":sub,"variants":[{"id":Uuid::now_v7(),"locale":"en","content":{"version":2,"text":"The plane takes off.","annotations":[]}}]}],
        "senses":[{"id":Uuid::now_v7(),"part_of_speech":"phrase_verbal_constituent","sub_pos":sub,"level":"A1","sense_group_id":group,
            "definitions":[{"id":Uuid::now_v7(),"locale":"en","text":"to begin flying","style":"definition","level":"A1","grammar_structure_id":grammar},{"id":Uuid::now_v7(),"locale":"zh-Hans","text":"起飞","style":"definition","level":"A1","grammar_structure_id":grammar}],
            "equivalents":[{"id":Uuid::now_v7(),"locale":"zh-Hans","text":"起飞"}]}]});
    let (status, saved) = full_call(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let mut invalid = draft.clone();
    invalid["senses"][0]["part_of_speech"] = json!("phrase_unknown");
    let (status, _) = full_call(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":invalid}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let preview = checked(&state, actor, &path, 2, 1).await;
    assert_eq!(preview["can_publish"], true, "{preview}");
    let (status, published) = full_call(
        &state,
        actor,
        "POST",
        &format!("{path}/publications"),
        command(&preview),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    let (_, publication_state) = full_call(
        &state,
        actor,
        "GET",
        &format!("{path}/publication"),
        Value::Null,
    )
    .await;
    assert_eq!(publication_state["status"], "published");
    assert_eq!(
        publication_state["current"]["snapshot"]["draft"]["senses"][0]["part_of_speech"],
        "phrase_verbal_constituent"
    );
    let (status, page) = full_call(
        &state,
        actor,
        "GET",
        "/workspace/entries?pos=phrase_verbal_constituent&kind=phrase&level=A1",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["items"][0]["levels"], json!(["A1"]));
    let (_, legacy) = call(
        &state,
        Some(actor),
        "GET",
        "/workspace/entries",
        Value::Null,
    )
    .await;
    assert!(legacy["items"][0].get("levels").is_none());
}

async fn dialect_save(
    state: &AppState,
    actor: Uuid,
    path: &str,
    entry: &Value,
    usage: &str,
    target: Option<&Value>,
) -> (StatusCode, Value) {
    let old = &entry["dialect_senses"][0]["target"];
    let target=target.map(|v|json!({"entry_id":v["id"],"revision":v["revision"],"sense_id":v["draft"]["senses"][0]["id"]}));
    full_call(state,actor,"PUT",path,json!({"expected_revision":entry["revision"],"draft":entry["draft"],"dialect_changes":[{"sense_id":entry["draft"]["senses"][0]["id"],"usage":usage,"target":target,"previous_target_revision":old["revision"]}]})).await
}
async fn publish_dialect(
    state: &AppState,
    actor: Uuid,
    path: &str,
    revision: i64,
    lifecycle: i64,
) -> Value {
    let p = checked(state, actor, path, revision, lifecycle).await;
    assert_eq!(p["can_publish"], true, "{p}");
    let (status, rec) = full_call(
        state,
        actor,
        "POST",
        &format!("{path}/publications"),
        command(&p),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rec}");
    p
}
#[sqlx::test]
async fn dialect_pairs_publish_independently_and_never_resolve_a_draft(pool: PgPool) {
    let down = include_str!("../../migrations/20261116160000_lexicon_next_dialect_pairs.down.sql");
    sqlx::raw_sql(down).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261116160000_lexicon_next_dialect_pairs.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let actor = admin(&pool, PUBLISH).await;
    let state = AppState::for_test(pool.clone());
    let (brit, a) = fixture(&state, &pool, actor).await;
    let (amer, b) = fixture(&state, &pool, actor).await;
    let (ok, b) = dialect_save(&state, actor, &amer, &b, "en-US", None).await;
    assert_eq!(ok, StatusCode::OK, "{b}");
    let (ok, a) = dialect_save(&state, actor, &brit, &a, "en-GB", Some(&b)).await;
    assert_eq!(ok, StatusCode::OK, "{a}");
    let (_, b) = full_call(&state, actor, "GET", &amer, Value::Null).await;
    assert_eq!(
        a["dialect_senses"][0]["target"]["sense_id"],
        b["draft"]["senses"][0]["id"]
    );
    assert_eq!(
        b["dialect_senses"][0]["target"]["sense_id"],
        a["draft"]["senses"][0]["id"]
    );
    let read = format!("{brit}/publication");
    let p = publish_dialect(&state, actor, &brit, 3, 1).await;
    assert!(
        p["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["field"] == "dialect_pairs")
    );
    let (_, first) = full_call(&state, actor, "GET", &read, Value::Null).await;
    assert_eq!(first["dialect_senses"][0]["paired"], true);
    assert!(first["dialect_senses"][0]["target"].is_null());
    publish_dialect(&state, actor, &amer, 4, 1).await;
    let (_, live) = full_call(&state, actor, "GET", &read, Value::Null).await;
    assert_eq!(live["dialect_senses"][0]["target"]["headword"], "center");
    assert_eq!(live["current"], first["current"]);
    for (action, lifecycle) in [("archive", 2), ("restore", 3)] {
        let (ok,out)=full_call(&state,actor,"POST","/workspace/commands",json!({"action":action,"request_id":Uuid::now_v7(),"items":[{"entry_id":b["id"],"base_revision":4,"base_lifecycle_revision":lifecycle}]})).await;
        assert_eq!(ok, StatusCode::OK, "{out}");
        assert_eq!(full_call(&state,actor,"GET",&read,Value::Null).await.1["dialect_senses"][0]["target"].is_null(),action=="archive");
    }
    // Old-client save: no pair field/command. It cannot erase the reciprocal edge.
    let (ok, legacy) = full_call(
        &state,
        actor,
        "PUT",
        &format!("{brit}?legacy"),
        json!({"expected_revision":3,"draft":a["draft"]}),
    )
    .await;
    assert_eq!(ok, StatusCode::OK);
    assert!(legacy.get("dialect_senses").is_none());
    let (_, a) = full_call(&state, actor, "GET", &brit, Value::Null).await;
    assert_eq!(a["dialect_senses"][0]["target"]["entry_id"], b["id"]);
    let (ok, a) = dialect_save(&state, actor, &brit, &a, "en-GB", None).await;
    assert_eq!(ok, StatusCode::OK, "{a}");
    assert_eq!(
        full_call(&state, actor, "GET", &read, Value::Null).await.1["dialect_senses"],
        live["dialect_senses"]
    );
    publish_dialect(&state, actor, &brit, 5, 2).await;
    let (_, other) = full_call(
        &state,
        actor,
        "GET",
        &format!("{amer}/publication"),
        Value::Null,
    )
    .await;
    assert!(other["dialect_senses"][0]["target"].is_null());
    assert_eq!(
        full_call(&state, actor, "GET", &amer, Value::Null).await.1["dialect_senses"][0]["paired"],
        false
    );
    assert!(sqlx::raw_sql(down).execute(&pool).await.is_err());
}

#[sqlx::test]
async fn dialect_pairs_conflict_and_permissions_leave_both_sides_intact(pool: PgPool) {
    let actor = admin(&pool, PUBLISH).await;
    let stranger = admin(&pool, PUBLISH).await;
    let state = AppState::for_test(pool.clone());
    let (target, b) = fixture(&state, &pool, actor).await;
    let (ok, b) = dialect_save(&state, actor, &target, &b, "en-US", None).await;
    assert_eq!(ok, StatusCode::OK);
    let (left, a) = fixture(&state, &pool, actor).await;
    let (right, c) = fixture(&state, &pool, actor).await;
    let (foreign, f) = fixture(&state, &pool, stranger).await;
    assert_eq!(
        dialect_save(&state, stranger, &foreign, &f, "en-GB", Some(&b))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        dialect_save(&state, actor, &left, &a, "en-US", Some(&b))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (one, two) = tokio::join!(
        dialect_save(&state, actor, &left, &a, "en-GB", Some(&b)),
        dialect_save(&state, actor, &right, &c, "en-GB", Some(&b))
    );
    assert!(
        matches!(
            (one.0, two.0),
            (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
        ),
        "{one:?} {two:?}"
    );
    let (winner, loser) = if one.0 == StatusCode::OK {
        (&left, &right)
    } else {
        (&right, &left)
    };
    let (_, w) = full_call(&state, actor, "GET", winner, Value::Null).await;
    let (_, l) = full_call(&state, actor, "GET", loser, Value::Null).await;
    assert_eq!(l["revision"], 2);
    let (_, b) = full_call(&state, actor, "GET", &target, Value::Null).await;
    assert_eq!(b["dialect_senses"][0]["target"]["entry_id"], w["id"]);
    assert_eq!(
        dialect_save(&state, actor, winner, &w, "en-GB", Some(&l))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        full_call(&state, actor, "GET", &target, Value::Null)
            .await
            .1,
        b
    );
    let (status, l) = dialect_save(&state, actor, loser, &l, "en-US", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, b_updated) = full_call(
        &state,
        actor,
        "PUT",
        &target,
        json!({"expected_revision":b["revision"],"draft":b["draft"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The source is unchanged, but unlinking would overwrite an unseen peer version.
    assert_eq!(
        dialect_save(&state, actor, winner, &w, "en-GB", Some(&l))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (_, w) = full_call(&state, actor, "GET", winner, Value::Null).await;
    assert_eq!(
        w["dialect_senses"][0]["target"]["revision"],
        b_updated["revision"]
    );
    let (status, replaced) = dialect_save(&state, actor, winner, &w, "en-GB", Some(&l)).await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    let (_, previous) = full_call(&state, actor, "GET", &target, Value::Null).await;
    assert_eq!(previous["dialect_senses"][0]["paired"], false);
    assert_eq!(previous["dialect_senses"][0]["no_equivalent"], false);
    let (_, new_peer) = full_call(&state, actor, "GET", loser, Value::Null).await;
    assert_eq!(new_peer["dialect_senses"][0]["target"]["entry_id"], w["id"]);
    let (status,none)=full_call(&state,actor,"PUT",&target,json!({"expected_revision":previous["revision"],"draft":previous["draft"],"dialect_changes":[{"sense_id":previous["draft"]["senses"][0]["id"],"usage":"en-US","no_equivalent":true,"note":"已人工核对"}]})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(none["dialect_senses"][0]["note"], "已人工核对");
}

#[sqlx::test]
async fn dialect_pair_reapplying_same_target_preserves_identity_and_peer_revision(pool: PgPool) {
    let actor = admin(&pool, PUBLISH).await;
    let state = AppState::for_test(pool.clone());
    let (source_path, source) = fixture(&state, &pool, actor).await;
    let (peer_path, peer) = fixture(&state, &pool, actor).await;

    let (status, peer) = dialect_save(&state, actor, &peer_path, &peer, "en-US", None).await;
    assert_eq!(status, StatusCode::OK, "{peer}");
    let (status, source) =
        dialect_save(&state, actor, &source_path, &source, "en-GB", Some(&peer)).await;
    assert_eq!(status, StatusCode::OK, "{source}");

    let source_id: Uuid = serde_json::from_value(source["id"].clone()).unwrap();
    let (_, peer_after_pair) = full_call(&state, actor, "GET", &peer_path, Value::Null).await;
    let pair_id_before: Uuid = sqlx::query_scalar(
        "SELECT (item->>'pair_id')::uuid FROM lexicon_next.draft_revisions r, jsonb_array_elements(r.dialect_senses) item WHERE r.entry_id=$1 AND r.revision=$2",
    )
    .bind(source_id)
    .bind(source["revision"].as_i64().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    let pair_count_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon_next.dialect_pairs")
            .fetch_one(&pool)
            .await
            .unwrap();

    let (status, repeated) = dialect_save(
        &state,
        actor,
        &source_path,
        &source,
        "en-GB",
        Some(&peer_after_pair),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repeated}");

    let (_, peer_after_repeat) = full_call(&state, actor, "GET", &peer_path, Value::Null).await;
    assert_eq!(peer_after_repeat["revision"], peer_after_pair["revision"]);
    let pair_id_after: Uuid = sqlx::query_scalar(
        "SELECT (item->>'pair_id')::uuid FROM lexicon_next.draft_revisions r, jsonb_array_elements(r.dialect_senses) item WHERE r.entry_id=$1 AND r.revision=$2",
    )
    .bind(source_id)
    .bind(repeated["revision"].as_i64().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pair_id_after, pair_id_before);
    let pair_count_after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon_next.dialect_pairs")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pair_count_after, pair_count_before);
}

#[sqlx::test]
async fn dialect_projects_distinct_senses_from_one_peer_revision(pool: PgPool) {
    let actor = admin(&pool, PUBLISH).await;
    let state = AppState::for_test(pool.clone());
    let (source, a) = fixture(&state, &pool, actor).await;
    let (peer, b) = fixture(&state, &pool, actor).await;
    let mut docs = [a["draft"].clone(), b["draft"].clone()];
    for doc in &mut docs {
        let second = json!({"id":Uuid::now_v7(),"part_of_speech":"noun","definitions":[],"equivalents":[{"id":Uuid::now_v7(),"locale":"zh-Hans","text":"第二义"}]});
        doc["senses"].as_array_mut().unwrap().push(second);
    }
    let targets: Vec<_> = docs[1]["senses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| json!({"sense_id":s["id"],"usage":"en-US"}))
        .collect();
    let (ok, b) = full_call(
        &state,
        actor,
        "PUT",
        &peer,
        json!({"expected_revision":b["revision"],"draft":docs[1],"dialect_changes":targets}),
    )
    .await;
    assert_eq!(ok, StatusCode::OK, "{b}");
    let links:Vec<_>=docs[0]["senses"].as_array().unwrap().iter().zip(targets).map(|(s,t)|json!({"sense_id":s["id"],"usage":"en-GB","target":{"entry_id":b["id"],"sense_id":t["sense_id"],"revision":b["revision"]}})).collect();
    let (ok, result) = full_call(
        &state,
        actor,
        "PUT",
        &source,
        json!({"expected_revision":a["revision"],"draft":docs[0],"dialect_changes":links}),
    )
    .await;
    assert_eq!(ok, StatusCode::OK, "{result}");
    for (i, link) in links.iter().enumerate() {
        let row = result["dialect_senses"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["sense_id"] == link["sense_id"])
            .unwrap();
        assert_eq!(row["target"]["sense_id"], link["target"]["sense_id"]);
        assert_eq!(row["target"]["revision"], 4);
        assert_eq!(
            row["target"]["gloss"],
            if i == 0 { "中心" } else { "第二义" }
        );
    }
}

#[sqlx::test]
async fn form_question_eligibility_survives_legacy_writes_and_freezes_at_publication(pool: PgPool) {
    let actor = admin(&pool, PUBLISH).await;
    sqlx::query("UPDATE catalog.form_types SET default_question_eligible=false WHERE code='base'")
        .execute(&pool)
        .await
        .unwrap();
    let state = AppState::for_test(pool.clone());
    let (path, saved) = fixture(&state, &pool, actor).await;
    let form_id = &saved["draft"]["form_editor"]["pos"][0]["forms"][0]["id"];
    let disabled = json!([{"form_id": form_id, "eligible": false}]);
    assert_eq!(saved["draft"]["form_question_eligibility"], disabled);
    sqlx::query("UPDATE catalog.form_types SET default_question_eligible=true WHERE code='base'")
        .execute(&pool)
        .await
        .unwrap();
    let legacy_path = format!("{path}?legacy-questions");
    let (status, legacy) = full_call(&state, actor, "GET", &legacy_path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(legacy["draft"].get("form_question_eligibility").is_none());
    let (status, saved_legacy) = full_call(
        &state,
        actor,
        "PUT",
        &legacy_path,
        json!({"expected_revision":saved["revision"],"draft":legacy["draft"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved_legacy}");
    let (status, edited) = full_call(&state, actor, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(edited["draft"]["form_question_eligibility"], disabled);
    let preview = checked(
        &state,
        actor,
        &path,
        edited["revision"].as_i64().unwrap(),
        1,
    )
    .await;
    let (status, published) = full_call(
        &state,
        actor,
        "POST",
        &format!("{path}/publications"),
        command(&preview),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(
        published["snapshot"]["draft"]["form_question_eligibility"],
        disabled
    );
    let mut draft = edited["draft"].clone();
    draft["form_question_eligibility"][0]["form_id"] = json!(Uuid::now_v7());
    assert_eq!(
        full_call(
            &state,
            actor,
            "PUT",
            &path,
            json!({"expected_revision":edited["revision"],"draft":draft})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    draft["form_question_eligibility"] = json!([{"form_id":form_id,"eligible":true}]);
    let (status, enabled) = full_call(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":edited["revision"],"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert!(enabled["draft"].get("form_question_eligibility").is_none());
    let (_, current) = full_call(
        &state,
        actor,
        "GET",
        &format!("{path}/publication"),
        Value::Null,
    )
    .await;
    assert_eq!(
        current["current"]["snapshot"]["draft"]["form_question_eligibility"],
        disabled
    );
}

#[sqlx::test]
async fn reviewed_publication_enters_mixed_wordlist_and_revocation_blocks_new_answers(
    pool: PgPool,
) {
    use crate::{
        account_deletion_support::call as student_call, learning_tasks_support as learning,
        wordlists_support as lists,
    };
    let (state, auth, _) = learning::setup(&pool).await;
    let author = admin(&pool, PUBLISH).await;
    let reviewer = admin(&pool, &["words.access", "words.review_assessment"]).await;
    let (path, saved) = fixture(&state, &pool, author).await;
    let entry: Uuid = serde_json::from_value(saved["id"].clone()).unwrap();
    let preview = checked(&state, author, &path, 2, 1).await;
    let (_, p) = full_call(
        &state,
        author,
        "POST",
        &format!("{path}/publications"),
        command(&preview),
    )
    .await;
    let url = format!("{path}/assessment");
    let (status, pending) = full_call(&state, author, "GET", &url, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{pending}");
    assert_eq!(pending["approved"], false, "{pending}");
    let mut decision = json!({"publication_id":p["id"],"lifecycle_revision":pending["lifecycle_revision"],"expected_version":0,"approved":true});
    let search = "/api/v1/wordlists/suggestions?q=centre";
    let pre = student_call(&state, &auth, "GET", search, Value::Null).await;
    assert_eq!(pre.0, 200, "{}", pre.1);
    assert_eq!(pre.1["items"], json!([]));
    assert_eq!(
        full_call(&state, author, "PUT", &url, decision.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (s, approved) = full_call(&state, reviewer, "PUT", &url, decision.clone()).await;
    assert_eq!(s, StatusCode::OK, "{approved}");
    assert_eq!(approved["approved"], true);
    let idx: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM learning_source.next_choice_candidates WHERE entry_id=$1",
    )
    .bind(entry)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(idx > 0);
    let found = student_call(&state, &auth, "GET", search, Value::Null).await;
    assert_eq!(found.1["items"][0]["entry_id"], saved["id"]);
    let typo = student_call(
        &state,
        &auth,
        "GET",
        "/api/v1/wordlists/suggestions?q=centtre",
        Value::Null,
    )
    .await;
    assert_eq!(typo.1["items"][0]["entry_id"], saved["id"], "{}", typo.1);
    let old = lists::full_entry(&pool, "legacy").await;
    let (s, list) = student_call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        lists::create_body(&[old, entry]),
    )
    .await;
    assert_eq!(s, 200, "{list}");
    let id: Uuid = serde_json::from_value(list["id"].clone()).unwrap();
    let (_, rows) = student_call(
        &state,
        &auth,
        "GET",
        &format!("/api/v1/me/wordlists/{id}/items?view=full"),
        Value::Null,
    )
    .await;
    assert_eq!(rows["items"].as_array().unwrap().len(), 2, "{rows}");
    assert_eq!(rows["items"][1]["entry"]["publication_id"], p["id"]);
    let task = learning::create(&state, &auth, id, "longterm").await;
    let run = learning::start(&state, &auth, &task).await;
    let questions = learning::questions(&state, &auth, &run).await;
    let new = questions["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["prompt"]["definition"] == "某物的中心位置")
        .expect("reviewed source is included");
    let (s, answer) = student_call(
        &state,
        &auth,
        "POST",
        &learning::answer_path(&run),
        learning::answer_body(new, "centre"),
    )
    .await;
    assert_eq!(s, 200, "{answer}");
    assert_eq!(answer["is_correct"], true);
    decision["approved"] = json!(false);
    assert_eq!(
        full_call(&state, reviewer, "PUT", &url, decision.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    decision["expected_version"] = approved["version"].clone();
    let (s, revoked) = full_call(&state, reviewer, "PUT", &url, decision).await;
    assert_eq!(s, StatusCode::OK, "{revoked}");
    assert_eq!(revoked["approved"], false);
    assert_eq!(
        student_call(&state, &auth, "GET", search, Value::Null)
            .await
            .1["items"],
        json!([])
    );
    let (_, rows) = student_call(
        &state,
        &auth,
        "GET",
        &format!("/api/v1/me/wordlists/{id}/items"),
        Value::Null,
    )
    .await;
    assert!(rows["items"][1]["entry"].is_null());
    let fresh = learning::create(&state, &auth, id, "longterm").await;
    let fresh_run = learning::start(&state, &auth, &fresh).await;
    let fresh_questions = learning::questions(&state, &auth, &fresh_run).await;
    assert!(
        fresh_questions["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["prompt"]["definition"] != "某物的中心位置")
    );
    let history: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM learning_answers WHERE question_id=$1 AND is_correct",
    )
    .bind(new["id"].as_str().unwrap().parse::<Uuid>().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(history, 1);
}

#[sqlx::test]
async fn republish_requires_new_assessment_and_closed_forms_remain_readable(pool: PgPool) {
    use crate::account_deletion_support::{call as student_call, setup_bound};
    let (state, student) = setup_bound(&pool).await;
    let author = admin(&pool, PUBLISH).await;
    let reviewer = admin(&pool, &["words.access", "words.review_assessment"]).await;
    let (path, saved) = fixture(&state, &pool, author).await;
    let review_url = format!("{path}/assessment");
    let publish_url = format!("{path}/publications");
    let preview = checked(&state, author, &path, 2, 1).await;
    let (status, first) = full_call(&state, author, "POST", &publish_url, command(&preview)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, initial) = full_call(&state, reviewer, "GET", &review_url, Value::Null).await;
    let review = |id: &Value, state: &Value| json!({"publication_id":id,"lifecycle_revision":state["lifecycle_revision"],"expected_version":state["version"],"approved":true});
    let (status, approved) = full_call(
        &state,
        reviewer,
        "PUT",
        &review_url,
        review(&first["id"], &initial),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    let mut draft = saved["draft"].clone();
    let form = draft["form_editor"]["pos"][0]["forms"][0]["id"].clone();
    draft["form_question_eligibility"] = json!([{"form_id":form,"eligible":false}]);
    let (status, updated) = full_call(
        &state,
        author,
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let preview = checked(&state, author, &path, 3, 2).await;
    let (_, second) = full_call(&state, author, "POST", &publish_url, command(&preview)).await;
    let (_, pending) = full_call(&state, reviewer, "GET", &review_url, Value::Null).await;
    assert_eq!(pending["approved"], false);
    assert_eq!(
        full_call(
            &state,
            reviewer,
            "PUT",
            &review_url,
            review(&first["id"], &approved)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, ok) = full_call(
        &state,
        reviewer,
        "PUT",
        &review_url,
        review(&second["id"], &pending),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ok}");
    let matches = student_call(
        &state,
        &student,
        "POST",
        "/api/v1/wordlists/import-matches",
        json!({"words":["centre","center"]}),
    )
    .await;
    assert_eq!(matches.0, 200, "{}", matches.1);
    for row in matches.1["items"].as_array().unwrap() {
        assert_eq!(row["status"], "matched");
        assert_eq!(row["candidates"][0]["publication_id"], second["id"]);
        assert!(row["candidates"][0].get("source_library").is_none());
    }
    let available: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM learning_source.choice_candidates WHERE entry_id=$1",
    )
    .bind(saved["id"].as_str().unwrap().parse::<Uuid>().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(available, 0);
    assert!(
        sqlx::raw_sql(include_str!(
            "../../migrations/20261126020000_assessed_learning_sources.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::raw_sql(include_str!(
            "../../migrations/20261126010000_form_question_eligibility.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
}
