use super::*;

#[sqlx::test]
async fn related_suggestions_lazy_senses_bind_and_persist_current_publication(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let ready = create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &[]).await;
    let (status, published) = publish_ready_v3(&state, &bearer, &ready).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    let target = published["word"]["id"].as_str().unwrap();
    let route = format!("{ROOT}/entries/related-suggestions?q=harb");
    let (status, result) = call(&state, Method::GET, &route, &bearer, None, None).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    let candidate = &result["items"][0];
    assert_eq!(candidate["entry_id"], target);
    assert_eq!(candidate["sense_count"], 1);
    assert!(candidate.get("senses").is_none());
    assert!(candidate.get("matches").is_none());
    assert!(candidate.get("snapshot").is_none());
    assert!(result.get("total").is_none());
    let pub_id = candidate["publication_id"].as_str().unwrap();
    let path = format!("{ROOT}/entries/{target}/relation-senses?publication_id={pub_id}");
    let (status, senses) = call(&state, Method::GET, &path, &bearer, None, None).await;
    assert_eq!(status, StatusCode::OK, "{senses}");
    assert_eq!(senses["senses"][0]["gloss"], "港口");
    let target_sense = &senses["senses"][0]["sense_id"];
    assert_eq!(
        call(
            &state,
            Method::GET,
            &format!("{route}&exclude_entry_id={target}"),
            &bearer,
            None,
            None
        )
        .await
        .1["items"],
        json!([])
    );
    let relation = json!({"id":Uuid::now_v7(),"relation":"synonym","score":"80.00","target_word_id":target,"target_sense_id":target_sense});
    let saved = create_v3_word_with_relation(&state, &bearer, "quayside", relation).await;
    let source = saved["word"]["id"].as_str().unwrap();
    let (status, reopened) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    assert_eq!(
        reopened["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_word_id"],
        target
    );
    assert_eq!(
        reopened["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_sense_id"],
        *target_sense
    );
    let (_, current) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{target}"),
        &bearer,
        None,
        None,
    )
    .await;
    let mut meanings = writable_v3_meanings(&current);
    meanings["pos"][0]["senses"][0]["definitions"][0]["content"] = rich_text("新的港口释义");
    let updated = save_v3_meanings(&state, &bearer, &current, meanings).await;
    let (status, _) = publish_ready_v3(&state, &bearer, &updated).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        call(&state, Method::GET, &path, &bearer, None, None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (_, fresh) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{target}/relation-senses"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(fresh["senses"][0]["gloss"], "新的港口释义");
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=$1")
        .bind(Uuid::parse_str(target).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, Method::GET, &path, &bearer, None, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn related_suggestions_use_words_permission_and_respect_v3_gate(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let actor = seed_admin_with_role(&pool, AdminRole::Admin).await;
    let bearer = token(&state, actor);
    let path = format!("{ROOT}/entries/related-suggestions?q=harb");
    assert_eq!(
        call(&state, Method::GET, &path, &bearer, None, None)
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query(
        "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='words.access'",
    )
    .bind(actor)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        call(&state, Method::GET, &path, &bearer, None, None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &state,
            Method::GET,
            &format!("{ROOT}/entries/{}/relation-senses", Uuid::now_v7()),
            &bearer,
            None,
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let super_token = token(&state, seed_admin(&pool).await);
    let state = state.with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_disabled());
    assert_eq!(
        call(&state, Method::GET, &path, &super_token, None, None)
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
