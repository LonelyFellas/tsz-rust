use super::*;

fn assert_legacy_word(word: &Value) {
    for variant in word["forms"]["pos"][0]["forms"][0]["regional_variants"]
        .as_object()
        .unwrap()
        .values()
        .filter(|value| value.is_object())
    {
        assert!(variant.get("spelling_rich").is_none(), "{word}");
    }
}

#[sqlx::test]
async fn markup_negotiation_preserves_cached_publish_history_and_lifecycle(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let ready = create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &[]).await;
    let id = ready["word"]["id"].as_str().unwrap();
    let region = "/pos/0/forms/0/regional_variants/uk";
    let mut forms = ready["word"]["forms"].clone();
    let variant = forms.pointer_mut(region).unwrap();
    variant["spelling_rich"] = json!({"version":2,"text":variant["spelling"],"annotations":[{"type":"italic","start":0,"end":2}]});
    let (_, saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        id,
        ready["word"]["revision"].as_i64().unwrap(),
        "complete",
        forms,
    )
    .await;
    let expected = saved["word"]["forms"].clone();

    let (status, meanings) = call_with_spelling_markup(&state, Method::PUT,
        &format!("{ROOT}/entries/{id}/steps/meanings"), &bearer, None,
        Some(json!({"schema_version":3,"base_revision":saved["word"]["revision"],"intent":"complete","content":writable_v3_meanings(&saved)})), false).await;
    assert_eq!(status, StatusCode::OK, "{meanings}");
    assert_legacy_word(&meanings["word"]);
    let (_, loaded) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(loaded["word"]["forms"], expected);

    let key = Uuid::now_v7();
    let input = json!({"schema_version":3,"base_revision":loaded["word"]["revision"]});
    let uri = format!("{ROOT}/entries/{id}/publications");
    let (status, published) = call_with_spelling_markup(
        &state,
        Method::POST,
        &uri,
        &bearer,
        Some(key),
        Some(input.clone()),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_legacy_word(&published["word"]);
    let entry_id = Uuid::parse_str(id).unwrap();
    let publication = current_publication_id(&pool, entry_id).await;
    let (status, replay) = call(&state, Method::POST, &uri, &bearer, Some(key), Some(input)).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["word"]["forms"], expected);
    assert_eq!(current_publication_id(&pool, entry_id).await, publication);

    for path in [format!("{uri}/{publication}"), uri] {
        let (status, old) =
            call_with_spelling_markup(&state, Method::GET, &path, &bearer, None, None, false).await;
        assert_eq!(status, StatusCode::OK, "{old}");
        assert_legacy_word(
            old.pointer("/publication/word")
                .unwrap_or(&old["publications"][0]["word"]),
        );
        let (_, new) = call(&state, Method::GET, &path, &bearer, None, None).await;
        assert_eq!(
            new.pointer("/publication/word/forms")
                .unwrap_or(&new["publications"][0]["word"]["forms"]),
            &expected
        );
    }

    let mut current = replay["word"].clone();
    for (operation, batch) in [
        ("archive", false),
        ("restore", true),
        ("archive", true),
        ("restore", false),
    ] {
        let input = json!({"base_revision":current["revision"],"base_lifecycle_revision":current["lifecycle_revision"]});
        let (path, body) = if batch {
            (
                format!("{ROOT}/entries/{operation}-batch"),
                json!({"entries":[{
                    "id":id,"base_revision":current["revision"],"base_lifecycle_revision":current["lifecycle_revision"]
                }]}),
            )
        } else {
            (format!("{ROOT}/entries/{id}/{operation}"), input)
        };
        let key = Uuid::now_v7();
        let (status, old) = call_with_spelling_markup(
            &state,
            Method::POST,
            &path,
            &bearer,
            Some(key),
            Some(body.clone()),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{old}");
        assert_legacy_word(if batch {
            &old["words"][0]
        } else {
            &old["word"]
        });
        let (status, new) = call(&state, Method::POST, &path, &bearer, Some(key), Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{new}");
        current = if batch {
            new["words"][0].clone()
        } else {
            new["word"].clone()
        };
        assert_eq!(current["forms"], expected);
    }

    let mut forms = current["forms"].clone();
    forms.pointer_mut(region).unwrap()["spelling_rich"]["annotations"][0]["end"] = json!(3);
    let (_, changed) = save_v3_forms_after_impact(
        &state,
        &bearer,
        id,
        current["revision"].as_i64().unwrap(),
        "complete",
        forms,
    )
    .await;
    let body = json!({"schema_version":3,"items":[{"entry_id":id,"base_revision":changed["word"]["revision"],"base_lifecycle_revision":changed["word"]["lifecycle_revision"]}]});
    let key = Uuid::now_v7();
    let path = format!("{ROOT}/entries/publications/batch");
    let (status, old) = call_with_spelling_markup(
        &state,
        Method::POST,
        &path,
        &bearer,
        Some(key),
        Some(body.clone()),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{old}");
    assert_legacy_word(&old["words"][0]);
    let (status, new) = call(&state, Method::POST, &path, &bearer, Some(key), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{new}");
    assert_eq!(new["words"][0]["forms"], changed["word"]["forms"]);
    assert_eq!(
        current_publication_snapshot(&pool, entry_id).await["forms"],
        changed["word"]["forms"]
    );
    let latest = &new["words"][0];
    let body = json!({"schema_version":3,"base_revision":latest["revision"],"base_lifecycle_revision":latest["lifecycle_revision"]});
    let path = format!("{ROOT}/entries/{id}/publications/{publication}/rollback");
    let key = Uuid::now_v7();
    let (status, old) = call_with_spelling_markup(
        &state,
        Method::POST,
        &path,
        &bearer,
        Some(key),
        Some(body.clone()),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{old}");
    assert_legacy_word(&old["word"]);
    let rolled = current_publication_id(&pool, entry_id).await;
    assert_eq!(
        current_publication_snapshot(&pool, entry_id).await["forms"],
        expected
    );
    let (status, new) = call(&state, Method::POST, &path, &bearer, Some(key), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{new}");
    assert_eq!(new["word"]["forms"], changed["word"]["forms"]);
    assert_eq!(current_publication_id(&pool, entry_id).await, rolled);
}
