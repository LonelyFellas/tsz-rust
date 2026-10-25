use super::*;
use axum::{
    body::Body,
    http::{Request, header},
};
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

#[sqlx::test]
async fn dictionary_requires_login_and_reads_only_current_publications(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let apple = full_entry(&pool, "apple").await;
    share_current_publication(&pool, apple).await;
    let other = entry(&pool, "pineapple").await;
    let hidden = entry(&pool, "apple-hidden").await;
    sqlx::query("UPDATE lexicon.entries SET archived_at=now() WHERE id=$1")
        .bind(hidden)
        .execute(&pool)
        .await
        .unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/dictionary?q=apple")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let (status, body) = call(
        &state,
        &auth,
        "GET",
        "/api/v1/dictionary?q=APPLE&page_size=1",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["pagination"]["total"], 2);
    assert_eq!(body["items"][0]["content"]["entry_id"], apple.to_string());
    assert!(body["items"][0]["content"]["pos"][0]["forms"].is_array());
    let rendered = body.to_string();
    for secret in [
        "ADMIN PRIVATE",
        "PRIVATE ACTUAL",
        "created_by",
        "detection_snapshot",
        "synthesis",
        "object_key",
    ] {
        assert!(!rendered.contains(secret), "leaked {secret}");
    }
    let (_, second) = call(
        &state,
        &auth,
        "GET",
        "/api/v1/dictionary?q=apple&page_size=1&page=2",
        Value::Null,
    )
    .await;
    assert_eq!(second["items"][0]["content"]["entry_id"], other.to_string());
    for q in ["", "?q=", "?q=apple&page=0", "?q=apple&page_size=101"] {
        assert_eq!(
            call(
                &state,
                &auth,
                "GET",
                &format!("/api/v1/dictionary{q}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let path = format!("/api/v1/dictionary/entries/{apple}");
    assert_eq!(
        call(&state, &auth, "GET", &path, Value::Null).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!("/api/v1/dictionary/entries/{hidden}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=NULL WHERE id=$1")
        .bind(apple)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &auth, "GET", &path, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, after) = call(
        &state,
        &auth,
        "GET",
        "/api/v1/dictionary?q=apple",
        Value::Null,
    )
    .await;
    assert_eq!(after["pagination"]["total"], 1);
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!(
                "/api/v1/dictionary/entries/{other}/audio/{}",
                Uuid::now_v7()
            ),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn dictionary_matches_published_surface_but_not_draft_or_old_publication(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let id = full_entry(&pool, "centre").await;
    let (publication, revision): (Uuid, i64) =
        sqlx::query_as("SELECT current_publication_id,revision FROM lexicon.entries WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let variant = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.surface_sources(entry_id,source_id,source_kind,source_node_id,language,entry_kind,dialect,dialect_scope,surface,normalized_surface,normalization_version,source_revision,content_scope,publication_id,pos_id,pos,form_type,content_schema_version,form_id,variant_id,group_ids,projection_version) VALUES($1,$2,'form_variant',$3,'en','word','us','us','center','center',1,$4,'current_publication',$5,$6,'noun','base',3,$7,$3,ARRAY[$8]::uuid[],'v3-test')").bind(id).bind(variant.to_string()).bind(variant).bind(revision).bind(publication).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    let (_, found) = call(
        &state,
        &auth,
        "GET",
        "/api/v1/dictionary?q=center",
        Value::Null,
    )
    .await;
    assert_eq!(found["pagination"]["total"], 1, "{found}");
    let manager = admin(&pool).await;
    publish(&pool, id, manager, "new spelling", 3).await;
    let (_, gone) = call(
        &state,
        &auth,
        "GET",
        "/api/v1/dictionary?q=center",
        Value::Null,
    )
    .await;
    assert_eq!(gone["pagination"]["total"], 0);
}

#[sqlx::test]
async fn dictionary_audio_signs_only_current_published_references(pool: PgPool) {
    use tsz_rust::platform::storage::{
        MemoryAdapter, StoragePolicy, StoragePrivacy, StorageRegistry, StorageSpace,
    };
    let (mut state, auth) = setup_bound(&pool).await;
    let store = MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(
            StoragePrivacy::Private,
            1024,
            std::time::Duration::from_secs(60),
            None,
        )
        .unwrap(),
    );
    state.object_storage = StorageRegistry::from_stores([store]).unwrap();
    let id = full_entry(&pool, "sound").await;
    let manager = admin(&pool).await;
    let asset = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id) VALUES($1,$2,$3,'audio/mpeg',10,'en-GB','female','sound.mp3',$4)").bind(asset).bind(format!("assets/{asset}.mp3")).bind(format!("uploads/{asset}.mp3")).bind(manager).execute(&pool).await.unwrap();
    let (publication, mut snapshot): (Uuid,Value) = sqlx::query_as("SELECT p.id,p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(id).fetch_one(&pool).await.unwrap();
    let unselected = Uuid::now_v7();
    snapshot["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["pronunciations"][0]["voice_profile"] = json!({"voices":[
        {"voice_id":"sonia","enabled":false,"rate_percent":-25,"version_id":Uuid::now_v7(),"version_name":"备选","locale":"en-GB","audio_asset_id":unselected},
        {"voice_id":"sonia","enabled":true,"rate_percent":0,"version_id":Uuid::now_v7(),"version_name":"正式","locale":"en-GB","audio_asset_id":asset}
    ]});
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=$1")
        .bind(publication)
        .bind(snapshot)
        .execute(&pool)
        .await
        .unwrap();
    let path = format!("/api/v1/dictionary/entries/{id}/audio/{asset}");
    let signed = call(&state, &auth, "GET", &path, Value::Null).await;
    assert_eq!(signed.0, StatusCode::OK, "{}", signed.1);
    assert_eq!(signed.1["url_expires_in_seconds"], 60);
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!("/api/v1/dictionary/entries/{id}/audio/{unselected}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let other = full_entry(&pool, "unrelated").await;
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!("/api/v1/dictionary/entries/{other}/audio/{asset}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE lexicon.entries SET archived_at=now() WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &auth, "GET", &path, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn dictionary_v3_keeps_native_regions_and_supplements_without_legacy_fallback(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let id = full_entry(&pool, "regions").await;
    let (publication, mut snapshot): (Uuid,Value) = sqlx::query_as("SELECT p.id,p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(id).fetch_one(&pool).await.unwrap();
    let version = |locale, enabled| json!({"voice_id":locale,"enabled":enabled,"rate_percent":0,"version_id":Uuid::now_v7(),"version_name":"自然","locale":locale,"audio_asset_id":Uuid::now_v7()});
    snapshot["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["pronunciations"][0]["voice_profile"] = json!({"voices":[version("en-AU",true),version("en-CA",true),version("en-IN",true),version("en-IE",true),version("en-GB",false)]});
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=$1")
        .bind(publication)
        .bind(snapshot)
        .execute(&pool)
        .await
        .unwrap();
    let path = format!("/api/v1/dictionary/entries/{id}");
    let token = state
        .token_manager
        .generate_with_version(auth.subject, "student", auth.security_version)
        .unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri(&path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("X-TSZ-Voice-Versions", "v2")
                .header("X-TSZ-Voice-Locales", "v1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let items = body["audio"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    for (locale, group) in [
        ("en-AU", "uk"),
        ("en-CA", "us"),
        ("en-IN", "other"),
        ("en-IE", "other"),
    ] {
        assert!(
            items
                .iter()
                .any(|item| item["locale"] == locale && item["accent_group"] == group)
        );
    }
    let old = call(&state, &auth, "GET", &path, Value::Null).await;
    assert_eq!(old.0, StatusCode::OK);
    assert_eq!(old.1["audio"], json!([]));
    let mut flash = version("en-US", true);
    flash["voice_id"] = json!("azure-hd-flash-test");
    let mut snapshot: Value =
        sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id=$1")
            .bind(publication)
            .fetch_one(&pool)
            .await
            .unwrap();
    snapshot["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["pronunciations"][0]["voice_profile"]
        ["voices"] = json!([flash]);
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=$1")
        .bind(publication)
        .bind(snapshot)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &auth, "GET", &path, Value::Null).await.1["audio"],
        json!([])
    );
    let response = tsz_rust::router(state)
        .oneshot(
            Request::builder()
                .uri(&path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("X-TSZ-Voice-Versions", "v2")
                .header("X-TSZ-Voice-Locales", "v1")
                .header("X-TSZ-Voice-Models", "v1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let response: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(response["audio"].as_array().unwrap().len(), 1);
    assert!(response["audio"][0].get("hd_flash").is_none());
}

#[sqlx::test]
async fn dictionary_shared_examples_respect_hides_and_withdrawal(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let id = full_entry(&pool, "apple").await;
    let manager = admin(&pool).await;
    let (publication, snapshot): (Uuid,Value) = sqlx::query_as("SELECT p.id,p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(id).fetch_one(&pool).await.unwrap();
    let sense = Uuid::parse_str(
        snapshot["meanings"]["pos"][0]["senses"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    sqlx::query("INSERT INTO lexicon.nodes(id,entry_id,node_type) VALUES($1,$2,'sense')")
        .bind(sense)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let sentence = Uuid::now_v7();
    let published = Uuid::now_v7();
    let rich = |text: &str| json!({"version":1,"text":text,"spans":[],"liaisons":[]});
    let content = json!({"sentence":{"id":sentence,"level":"A1","en_text":{"mode":"unified","common":{"id":Uuid::now_v7(),"origin":"manual","value":rich("An apple.")}},"zh_text_id":Uuid::now_v7(),"zh_text":rich("一个苹果。"),"zh_translations":[],"links":[]},"annotations":[]});
    sqlx::query("INSERT INTO lexicon.shared_sentences(id,content,create_digest,created_by_admin_id) VALUES($1,$2,'dictionary-test',$3)").bind(sentence).bind(&content).bind(manager).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO lexicon.shared_sentence_publications(id,sentence_id,publication_number,source_revision,snapshot,published_by_admin_id) VALUES($1,$2,1,1,$3,$4)").bind(published).bind(sentence).bind(content).bind(manager).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.shared_sentences SET current_publication_id=$2 WHERE id=$1")
        .bind(sentence)
        .bind(published)
        .execute(&pool)
        .await
        .unwrap();
    let target = json!({"state":"linked","target_entry_id":id,"target_sense_id":sense,"target_publication_id":publication});
    sqlx::query("INSERT INTO lexicon.shared_sentence_publication_annotations(publication_id,sentence_id,id,source_dialect,source_segments,target_entry_id,target_sense_id,target_ref) VALUES($1,$2,$3,'common',$4,$5,$6,$7)").bind(published).bind(sentence).bind(Uuid::now_v7()).bind(json!([{"start":3,"end":8,"surface":"apple"}])).bind(id).bind(sense).bind(target).execute(&pool).await.unwrap();
    let path = format!("/api/v1/dictionary/entries/{id}");
    let visible = call(&state, &auth, "GET", &path, Value::Null).await;
    assert_eq!(visible.0, StatusCode::OK, "{}", visible.1);
    assert_eq!(
        visible.1["examples"][0]["translation"]["text"],
        "一个苹果。"
    );
    sqlx::query("INSERT INTO lexicon.shared_sentence_publication_hides(publication_id,entry_id,sense_id,sentence_id) VALUES($1,$2,$3,$4)").bind(publication).bind(id).bind(sense).bind(sentence).execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, &auth, "GET", &path, Value::Null).await.1["examples"],
        json!([])
    );
    sqlx::query("DELETE FROM lexicon.shared_sentence_publication_hides WHERE publication_id=$1")
        .bind(publication)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE lexicon.shared_sentences SET withdrawn_at=now(),withdrawn_reason='test' WHERE id=$1").bind(sentence).execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, &auth, "GET", &path, Value::Null).await.1["examples"],
        json!([])
    );
}

#[sqlx::test]
async fn dictionary_audio_keeps_source_text_dialect_separate_from_voice_locale(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let id = full_entry(&pool, "sourceaudio").await;
    let (publication, mut snapshot): (Uuid, Value) = sqlx::query_as("SELECT p.id,p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(id).fetch_one(&pool).await.unwrap();
    let grammar = &mut snapshot["meanings"]["pos"][0]["grammar_structures"][0];
    let mut uk = grammar["variants"][0].clone();
    let mut us = uk.clone();
    let uk_asset = Uuid::now_v7();
    let us_asset = Uuid::now_v7();
    uk["dialect"] = json!("uk");
    uk["content"]["text"] = json!("colour");
    us["id"] = json!(Uuid::now_v7());
    us["dialect"] = json!("us");
    us["content"]["text"] = json!("color");
    for (variant, asset) in [(&mut uk, uk_asset), (&mut us, us_asset)] {
        variant["voice_profile"] = json!({"voices":[{"voice_id":"neerja","enabled":true,"rate_percent":0,"version_id":Uuid::now_v7(),"version_name":"印度补充","locale":"en-IN","audio_asset_id":asset}]});
    }
    grammar["variants"] = json!([uk, us]);
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=$1")
        .bind(publication)
        .bind(snapshot)
        .execute(&pool)
        .await
        .unwrap();
    let path = format!("/api/v1/dictionary/entries/{id}");
    let token = state
        .token_manager
        .generate_with_version(auth.subject, "student", auth.security_version)
        .unwrap();
    for models in [false, true] {
        let mut request = Request::builder()
            .uri(&path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header("X-TSZ-Voice-Versions", "v2")
            .header("X-TSZ-Voice-Locales", "v1");
        if models {
            request = request.header("X-TSZ-Voice-Models", "v1");
        }
        let response = tsz_rust::router(state.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let items = body["audio"].as_array().unwrap();
        let uk = items
            .iter()
            .find(|item| item["asset_id"] == uk_asset.to_string())
            .unwrap();
        let us = items
            .iter()
            .find(|item| item["asset_id"] == us_asset.to_string())
            .unwrap();
        assert_eq!(uk["locale"], "en-IN");
        assert_eq!(us["locale"], "en-IN");
        if models {
            assert_eq!(uk["source_dialect"], "uk");
            assert_eq!(us["source_dialect"], "us");
        } else {
            assert!(uk.get("source_dialect").is_none());
            assert!(us.get("source_dialect").is_none());
        }
    }
}
