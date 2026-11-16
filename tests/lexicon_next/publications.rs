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
    ] {
        req = req.header(format!("x-tsz-next-{header}"), "v1");
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
