use super::*;
use std::{sync::Arc, time::Duration};
use tsz_rust::platform::storage::{
    MemoryAdapter, ObjectContentType, ObjectKey, ObjectStore, PutOptions, StoragePolicy,
    StoragePrivacy, StorageRegistry, StorageSpace,
};

fn storage(state: &mut AppState) -> Arc<dyn ObjectStore> {
    let store = MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    );
    state.object_storage = StorageRegistry::from_stores([store.clone()]).unwrap();
    store
}

fn with_audio(source: Uuid, assets: Value) -> Value {
    let mut value = content(source);
    value["sentence"]["en_text"]["common"]["audio_assets"] = assets;
    value
}

async fn confirm_sentence_audio(state: &AppState, actor: Uuid, id: &str, revision: i64) {
    let (status, review) = call(
        state,
        actor,
        Method::POST,
        &format!("{ROOT}/{id}/publication-audio/check"),
        Some(json!({"base_revision":revision})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{review}");
    assert_eq!(review["manifest"]["ready"], true, "{review}");
    let (status, confirmed) = call(
        state,
        actor,
        Method::POST,
        &format!("{ROOT}/{id}/publication-audio/confirm"),
        Some(json!({"base_revision":revision,"manifest_hash":review["manifest"]["manifest_hash"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmed}");
    assert_eq!(confirmed["confirmed"], true);
}

async fn create(state: &AppState, actor: Uuid, source: Uuid, value: Value) -> (StatusCode, Value) {
    call(
        state,
        actor,
        Method::POST,
        ROOT,
        Some(json!({
            "source_entry_id":source,"source_sense_id":sense_id(source),"content":value
        })),
    )
    .await
}

#[sqlx::test]
async fn canonical_audio_metadata_and_invalid_claims_are_atomic(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool).await;
    let other = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let asset = audio_asset(&pool, actor).await;
    let foreign = audio_asset(&pool, other).await;
    let reclaiming = audio_asset(&pool, actor).await;
    sqlx::query("UPDATE lexicon.audio_assets SET reclamation_started_at=now() WHERE id=$1")
        .bind(reclaiming["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let mut unknown = asset.clone();
    unknown["id"] = json!(Uuid::now_v7());
    for assets in [
        json!([unknown]),
        json!([foreign]),
        json!([reclaiming]),
        json!([asset.clone(), asset.clone()]),
        json!(vec![asset.clone(); 9]),
    ] {
        let (status, body) = create(&state, actor, source, with_audio(source, assets)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentences")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "failed saves must not allocate a sentence or references"
    );
    let mut spoofed = asset.clone();
    spoofed["original_name"] = json!("forged.wav");
    spoofed["size_bytes"] = json!(999);
    let (status, saved) = create(&state, actor, source, with_audio(source, json!([spoofed]))).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["content"]["sentence"]["en_text"]["common"]["audio_assets"],
        json!([asset.clone()])
    );
    let (status, body) = create(&state, actor, source, with_audio(source, json!([asset]))).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "another sentence cannot claim it: {body}"
    );
}

#[sqlx::test]
async fn published_audio_survives_draft_removal_reclamation_and_history_restore(pool: PgPool) {
    let mut state = AppState::for_test(pool.clone());
    let store = storage(&mut state);
    let actor = admin(&pool).await;
    let editor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let asset = audio_asset(&pool, actor).await;
    let asset_id: Uuid = asset["id"].as_str().unwrap().parse().unwrap();
    let key = ObjectKey::parse(format!("assets/{asset_id}.wav")).unwrap();
    store
        .put(
            &key,
            vec![1, 2, 3],
            PutOptions::new(Some(ObjectContentType::parse("audio/wav").unwrap())),
        )
        .await
        .unwrap();
    let (status, created) = create(
        &state,
        actor,
        source,
        with_audio(source, json!([asset.clone()])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap();
    let mut value = created["content"].clone();
    value["annotations"] = json!([]); // Publication is independent of an unpublished source.
    let (status, updated) = call(
        &state,
        editor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":1,"content":value})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "another authorized editor retains attached audio: {updated}"
    );
    let (status, rejected) = call(
        &state,
        actor,
        Method::POST,
        &format!("{ROOT}/{id}/publications"),
        Some(json!({"base_revision":2,"base_lifecycle_revision":1})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    confirm_sentence_audio(&state, actor, id, 2).await;
    let (status, published) = call(
        &state,
        actor,
        Method::POST,
        &format!("{ROOT}/{id}/publications"),
        Some(json!({"base_revision":2,"base_lifecycle_revision":1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    let publication = published["current_publication_id"].as_str().unwrap();
    value["sentence"]["en_text"]["common"]["audio_assets"] = json!([]);
    let (status, cleared) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":2,"content":value})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert!(
        cleared["content"]["sentence"]["en_text"]["common"]
            .get("audio_assets")
            .is_none()
    );
    let (_, public) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}/{id}?view=published"),
        None,
    )
    .await;
    assert_eq!(
        public["content"]["sentence"]["en_text"]["common"]["audio_assets"],
        json!([asset.clone()])
    );
    sqlx::query("UPDATE lexicon.audio_assets SET created_at=now()-interval '8 days' WHERE id=$1")
        .bind(asset_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        tsz_rust::lexicon::audio_assets::reclaim_once(&pool, &store)
            .await
            .unwrap(),
        0
    );
    assert!(
        store.stat(&key).await.is_ok(),
        "history must keep the object alive"
    );
    let mut tx = pool.begin().await.unwrap();
    let error = sqlx::raw_sql(include_str!(
        "../../migrations/20261018140000_shared_sentence_audio.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("restore data without English audio"),
        "{error}"
    );
    tx.rollback().await.unwrap();
    let (_, impact) = call(
        &state,
        actor,
        Method::GET,
        &format!("{ROOT}/{id}/withdrawal-impact"),
        None,
    )
    .await;
    let (status, withdrawn) = call(&state, actor, Method::POST, &format!("{ROOT}/{id}/withdraw"), Some(json!({"base_revision":3,"base_lifecycle_revision":2,"reason":"test","impact_fingerprint":impact["fingerprint"]}))).await;
    assert_eq!(status, StatusCode::OK, "{withdrawn}");
    let (status, restored) = call(
        &state,
        editor,
        Method::POST,
        &format!("{ROOT}/{id}/publications/{publication}/rollback"),
        Some(json!({"base_revision":3,"base_lifecycle_revision":3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(
        restored["content"]["sentence"]["en_text"]["common"]["audio_assets"][0]["id"],
        asset["id"]
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.shared_sentence_audio_asset_references WHERE asset_id=$1",
    )
    .bind(asset_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        count, 2,
        "restored draft and immutable publication both retain ownership"
    );
}

#[sqlx::test]
async fn concurrent_sentences_cannot_share_an_audio_asset(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let asset = audio_asset(&pool, actor).await;
    let (left, right) = tokio::join!(
        create(
            &state,
            actor,
            source,
            with_audio(source, json!([asset.clone()]))
        ),
        create(
            &state,
            actor,
            source,
            with_audio(source, json!([asset.clone()]))
        )
    );
    assert!(
        (left.0 == StatusCode::OK && right.0 == StatusCode::BAD_REQUEST)
            || (right.0 == StatusCode::OK && left.0 == StatusCode::BAD_REQUEST),
        "{left:?} {right:?}"
    );
    let id: Uuid = asset["id"].as_str().unwrap().parse().unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.shared_sentence_audio_asset_references WHERE asset_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn cross_word_claims_are_rejected_and_published_readers_cannot_read_draft_audio(
    pool: PgPool,
) {
    let mut state = AppState::for_test(pool.clone());
    let store = storage(&mut state);
    let actor = admin(&pool).await;
    let reader = admin_with_role(&pool, AdminRole::Admin).await;
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1")
        .bind(reader)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'sentences.read_published',$1)").bind(reader).execute(&pool).await.unwrap();
    let source = entry(&pool, actor, "wonderful").await;
    let taken = audio_asset(&pool, actor).await;
    let taken_id: Uuid = taken["id"].as_str().unwrap().parse().unwrap();
    sqlx::query("INSERT INTO lexicon.v3_audio_asset_references(asset_id,entry_id,scope,variant_id) VALUES($1,$2,'draft',$3)")
        .bind(taken_id).bind(source).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    let (status, body) = create(&state, actor, source, with_audio(source, json!([taken]))).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "word-owned asset must not be shared: {body}"
    );
    let asset = audio_asset(&pool, actor).await;
    let asset_id: Uuid = asset["id"].as_str().unwrap().parse().unwrap();
    store
        .put(
            &ObjectKey::parse(format!("assets/{asset_id}.wav")).unwrap(),
            vec![1, 2, 3],
            PutOptions::new(Some(ObjectContentType::parse("audio/wav").unwrap())),
        )
        .await
        .unwrap();
    let (status, created) = create(&state, actor, source, with_audio(source, json!([asset]))).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let url = format!("/api/v1/admin/lexicon/audio-assets/{asset_id}/url");
    let (status, body) = call(&state, reader, Method::GET, &url, None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "draft audio remains private: {body}"
    );
    let error = sqlx::query("INSERT INTO lexicon.v3_audio_asset_references(asset_id,entry_id,scope,variant_id) VALUES($1,$2,'draft',$3)")
        .bind(asset_id).bind(source).bind(Uuid::now_v7()).execute(&pool).await.unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.constraint()),
        Some("audio_asset_owner")
    );
    let id = created["id"].as_str().unwrap();
    let mut value = created["content"].clone();
    value["annotations"] = json!([]);
    let (status, updated) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":1,"content":value})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    confirm_sentence_audio(&state, actor, id, 2).await;
    let (status, body) = call(
        &state,
        actor,
        Method::POST,
        &format!("{ROOT}/{id}/publications"),
        Some(json!({"base_revision":2,"base_lifecycle_revision":1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&state, reader, Method::GET, &url, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "published sentence read grant includes its audio: {body}"
    );
    value["sentence"]["en_text"]["common"]["audio_assets"] = json!([]);
    let (status, body) = call(
        &state,
        actor,
        Method::PUT,
        &format!("{ROOT}/{id}"),
        Some(json!({"base_revision":2,"content":value})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &state,
        actor,
        Method::POST,
        &format!("{ROOT}/{id}/publications"),
        Some(json!({"base_revision":3,"base_lifecycle_revision":2})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&state, reader, Method::GET, &url, None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "history alone cannot authorize a public reader: {body}"
    );
}

#[sqlx::test]
async fn empty_audio_requirements_do_not_require_a_storage_space(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let (status, empty) = create(&state, actor, source, content(source)).await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    let check = |sentence: &Value| {
        format!(
            "{ROOT}/{}/publication-audio/check",
            sentence["id"].as_str().unwrap()
        )
    };
    let (status, review) = call(
        &state,
        actor,
        Method::POST,
        &check(&empty),
        Some(json!({"base_revision":empty["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{review}");
    assert_eq!(review["manifest"]["ready"], true);
    assert_eq!(review["manifest"]["items"], json!([]));
    let asset = audio_asset(&pool, actor).await;
    let source_with_asset = entry(&pool, actor, "wonderful").await;
    let (status, with_asset) = create(
        &state,
        actor,
        source_with_asset,
        with_audio(source_with_asset, json!([asset])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{with_asset}");
    let (status, blocked) = call(
        &state,
        actor,
        Method::POST,
        &check(&with_asset),
        Some(json!({"base_revision":with_asset["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{blocked}");
    assert_eq!(blocked["code"], "audio_storage_not_configured");
}

#[sqlx::test]
async fn trash_retains_audio_reference_until_permanent_delete(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = admin(&pool).await;
    let source = entry(&pool, actor, "wonderful").await;
    let asset = audio_asset(&pool, actor).await;
    let (status, sentence) = create(
        &state,
        actor,
        source,
        with_audio(source, json!([asset.clone()])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sentence}");
    let id = sentence["id"].as_str().unwrap();
    let asset_id = asset["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    assert_eq!(
        call(
            &state,
            actor,
            Method::POST,
            &format!("{ROOT}/{id}/trash"),
            Some(json!({"base_revision":1}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let references: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.shared_sentence_audio_asset_references WHERE asset_id=$1",
    )
    .bind(asset_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(references, 1);
    assert_eq!(
        call(
            &state,
            actor,
            Method::POST,
            &format!("{ROOT}/{id}/untrash"),
            Some(json!({"base_revision":2}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (_, restored) = call(&state, actor, Method::GET, &format!("{ROOT}/{id}"), None).await;
    assert_eq!(restored["content"], sentence["content"]);
    assert_eq!(
        call(
            &state,
            actor,
            Method::POST,
            &format!("{ROOT}/{id}/trash"),
            Some(json!({"base_revision":3}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            &state,
            actor,
            Method::DELETE,
            &format!("{ROOT}/{id}"),
            Some(json!({"base_revision":4}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let references: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.shared_sentence_audio_asset_references WHERE asset_id=$1",
    )
    .bind(asset_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(references, 0);
}
