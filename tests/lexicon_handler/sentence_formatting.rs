use super::*;

fn has_new_formats(value: &Value) -> bool {
    if value.get("version").and_then(Value::as_u64) == Some(2)
        && value
            .get("annotations")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| matches!(item["type"].as_str(), Some("bold" | "underline")))
            })
    {
        return true;
    }
    match value {
        Value::Object(fields) => fields.values().any(has_new_formats),
        Value::Array(items) => items.iter().any(has_new_formats),
        _ => false,
    }
}

async fn audit_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit.admin_actions")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn sentence_formatting_negotiates_reads_and_protects_legacy_writes(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = forms["word"]["id"].as_str().unwrap();
    let meanings = complete_v3_meanings_fixture(forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    let path = format!("{ROOT}/entries/{id}/steps/meanings");
    let (status, plain) = call_with_capabilities(&state, Method::PUT, &path, &bearer, None,
        Some(json!({"schema_version":3,"base_revision":forms["word"]["revision"],"intent":"complete","content":meanings})), (true, false)).await;
    assert_eq!(status, StatusCode::OK, "{plain}");

    let mut meanings = writable_v3_meanings(&plain);
    let sense = &mut meanings["pos"][0]["senses"][0];
    let sense_id = sense["id"].clone();
    let grammar = sense["definitions"][0]["grammar_structure_id"].clone();
    let rich = json!({"version":2,"text":"a harbour today","annotations":[
        {"type":"bold","start":2,"end":9}, {"type":"underline","start":2,"end":9},
        {"type":"italic","start":3,"end":7}, {"type":"pause","at":1,"duration_ms":500}
    ]});
    sense["definitions"].as_array_mut().unwrap().push(json!({
        "definition_mode":"en_sentence","id":Uuid::now_v7(),"level":"A1","grammar_structure_id":grammar,
        "content":{"mode":"unified","common":{"id":Uuid::now_v7(),"origin":"manual","value":rich}}
    }));
    let (status, saved) = call(&state, Method::PUT, &path, &bearer, None,
        Some(json!({"schema_version":3,"base_revision":plain["word"]["revision"],"intent":"complete","content":meanings}))).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(has_new_formats(&saved));
    let (status, old) = call_with_capabilities(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        &bearer,
        None,
        None,
        (false, false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{old}");
    assert!(!has_new_formats(&old));
    let leaf = "/word/meanings/pos/0/senses/0/definitions/1/content/common/value/annotations";
    assert_eq!(
        old.pointer(leaf).unwrap(),
        &json!([
            {"type":"pause","at":1,"duration_ms":500},{"type":"italic","start":3,"end":7}
        ])
    );
    let audit = audit_count(&pool).await;
    let (status, rejected) = call_with_capabilities(&state, Method::PUT, &path, &bearer, None,
        Some(json!({"schema_version":3,"base_revision":old["word"]["revision"],"intent":"save","content":writable_v3_meanings(&old)})), (true, false)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert_eq!(
        rejected["field_issues"][0]["code"], "meanings_storage_unsafe",
        "{rejected}"
    );
    assert!(rejected["detail"].as_str().unwrap().contains("刷新"));
    assert_eq!(audit_count(&pool).await, audit);
    let (_, reread) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(reread["word"]["revision"], saved["word"]["revision"]);
    assert_eq!(reread["word"]["meanings"], saved["word"]["meanings"]);
    // Stale old saves retain the existing revision-conflict behavior and cannot drop the formats.
    let (status, _) = call_with_capabilities(&state, Method::PUT, &path, &bearer, None,
        Some(json!({"schema_version":3,"base_revision":plain["word"]["revision"],"intent":"save","content":writable_v3_meanings(&plain)})), (true, false)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, removed) = call_with_capabilities(&state, Method::PUT, &format!("{ROOT}/entries/{id}/steps/forms"), &bearer, None,
        Some(json!({"schema_version":3,"base_revision":saved["word"]["revision"],"intent":"save","content":{"pos":[]}})), (true, false)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{removed}");
    assert_eq!(
        removed["field_issues"][0]["code"], "meanings_storage_unsafe",
        "{removed}"
    );
    assert_eq!(audit_count(&pool).await, audit);
    // A normal old forms save uses canonical meanings and retains the new annotations.
    let (status, forms_saved) = call_with_capabilities(&state, Method::PUT, &format!("{ROOT}/entries/{id}/steps/forms"), &bearer, None,
        Some(json!({"schema_version":3,"base_revision":saved["word"]["revision"],"intent":"complete","content":saved["word"]["forms"]})), (true, false)).await;
    assert_eq!(status, StatusCode::OK, "{forms_saved}");
    let (_, current) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(current["word"]["meanings"], saved["word"]["meanings"]);
    let key = Uuid::now_v7();
    let publish_path = format!("{ROOT}/entries/{id}/publications");
    let input = json!({"schema_version":3,"base_revision":current["word"]["revision"]});
    let (status, old_published) = call_with_capabilities(
        &state,
        Method::POST,
        &publish_path,
        &bearer,
        Some(key),
        Some(input.clone()),
        (true, false),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{old_published}");
    assert!(!has_new_formats(&old_published));
    let (status, new_replay) = call(
        &state,
        Method::POST,
        &publish_path,
        &bearer,
        Some(key),
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{new_replay}");
    assert!(has_new_formats(&new_replay));
    let publication = current_publication_id(&pool, Uuid::parse_str(id).unwrap()).await;
    for path in [
        publish_path.clone(),
        format!("{publish_path}/{publication}"),
    ] {
        let (_, old) = call_with_capabilities(
            &state,
            Method::GET,
            &path,
            &bearer,
            None,
            None,
            (true, false),
        )
        .await;
        let (_, new) = call(&state, Method::GET, &path, &bearer, None, None).await;
        assert!(!has_new_formats(&old));
        assert!(has_new_formats(&new));
    }

    let form = &forms["word"]["forms"]["pos"][0]["forms"][0];
    let translation = Uuid::now_v7();
    let (status, sentence) = call(&state, Method::POST, &format!("{ROOT}/sentences"), &bearer, None, Some(json!({
        "source_entry_id":id,"source_sense_id":sense_id,
        "content":{"sentence":{"id":Uuid::now_v7(),"level":"A1","en_text":{"mode":"unified","common":{"id":Uuid::now_v7(),"origin":"manual","value":rich}},"zh_text_id":translation,"zh_text":rich_text("今天的港口"),"zh_translations":[{"id":translation,"band":"word_for_word","language":"zh","content":rich_text("今天的港口")}],"links":[]},
        "annotations":[{"id":Uuid::now_v7(),"source_dialect":"common","source_segments":[{"start":2,"end":9,"surface":"harbour"}],"target":{"state":"linked","target_entry_id":id,"target_pos_id":forms["word"]["forms"]["pos"][0]["pos_id"],"target_base_form_id":form["id"],"target_form_id":form["id"],"target_variant_id":form["regional_variants"]["uk"]["id"],"target_sense_id":sense_id}}]}
    }))).await;
    assert_eq!(status, StatusCode::OK, "{sentence}");
    let sid = sentence["id"].as_str().unwrap();
    let spath = format!("{ROOT}/sentences/{sid}");
    let (_, old_sentence) = call_with_capabilities(
        &state,
        Method::GET,
        &format!("{spath}?view=draft"),
        &bearer,
        None,
        None,
        (true, false),
    )
    .await;
    assert!(!has_new_formats(&old_sentence));
    let (status, rejected) = call_with_capabilities(
        &state,
        Method::PUT,
        &spath,
        &bearer,
        None,
        Some(json!({"base_revision":old_sentence["revision"],"content":old_sentence["content"]})),
        (true, false),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert!(rejected["detail"].as_str().unwrap().contains("刷新"));
    let (_, fresh_sentence) = call(
        &state,
        Method::GET,
        &format!("{spath}?view=draft"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(fresh_sentence, sentence);
    let key = Uuid::now_v7();
    let input = json!({"base_revision":sentence["revision"],"base_lifecycle_revision":sentence["lifecycle_revision"]});
    let (status, old_pub) = call_with_capabilities(
        &state,
        Method::POST,
        &format!("{spath}/publications"),
        &bearer,
        Some(key),
        Some(input.clone()),
        (true, false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{old_pub}");
    assert!(!has_new_formats(&old_pub));
    let (_, new_pub) = call(
        &state,
        Method::POST,
        &format!("{spath}/publications"),
        &bearer,
        Some(key),
        Some(input),
    )
    .await;
    assert!(has_new_formats(&new_pub));
    let pid = new_pub["current_publication_id"].as_str().unwrap();
    for path in [
        format!("{ROOT}/sentences?view=draft"),
        format!("{ROOT}/sentences?view=published"),
        spath.clone(),
        format!("{spath}/publications"),
        format!("{spath}/publications/{pid}"),
    ] {
        let (status, old) = call_with_capabilities(
            &state,
            Method::GET,
            &path,
            &bearer,
            None,
            None,
            (true, false),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{old}");
        assert!(!has_new_formats(&old));
        let (_, new) = call(&state, Method::GET, &path, &bearer, None, None).await;
        assert!(has_new_formats(&new), "{new}");
    }
    // Only a current client may intentionally clear formats from the stored sentence and word.
    let mut cleared = fresh_sentence["content"].clone();
    cleared["sentence"]["en_text"]["common"]["value"]["annotations"] = json!([]);
    let (status, clear) = call(
        &state,
        Method::PUT,
        &spath,
        &bearer,
        None,
        Some(json!({"base_revision":fresh_sentence["revision"],"content":cleared})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{clear}");
    assert!(!has_new_formats(&clear));
    let (_, latest) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        &bearer,
        None,
        None,
    )
    .await;
    let mut cleared = writable_v3_meanings(&latest);
    cleared["pos"][0]["senses"][0]["definitions"][1]["content"]["common"]["value"]["annotations"] =
        json!([]);
    let (status, clear) = call(&state, Method::PUT, &path, &bearer, None, Some(json!({"schema_version":3,"base_revision":latest["word"]["revision"],"intent":"complete","content":cleared}))).await;
    assert_eq!(status, StatusCode::OK, "{clear}");
    assert!(!has_new_formats(&clear));
}
