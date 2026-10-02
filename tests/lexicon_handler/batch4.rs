use super::*;

#[sqlx::test]
async fn batch4_published_summary_never_uses_draft_label(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let word = batch3_word(&state, &bearer, "alpha").await;
    let (status, _) = publish_ready_v3(&state, &bearer, &word).await;
    assert_eq!(status, StatusCode::CREATED);
    let draft = sentence(&state, &bearer, &word).await;
    publish_sentence(&state, &bearer, &draft).await;
    let before = public_list(&state, &bearer, None).await;
    sqlx::query("UPDATE lexicon.entry_presentation_projection SET label='unpublished spelling' WHERE entry_id=$1")
        .bind(Uuid::parse_str(word["word"]["id"].as_str().unwrap()).unwrap())
        .execute(&pool).await.unwrap();
    let after = public_list(&state, &bearer, None).await;
    assert_eq!(before["items"][0]["entries"], after["items"][0]["entries"]);
    assert_eq!(after["items"][0]["entries"][0]["headword"], "alpha");
}

#[sqlx::test]
async fn batch4_revocation_wins_against_waiting_publisher(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin_with_role(&pool, AdminRole::Admin).await;
    let bearer = token(&state, actor);
    let word = batch3_word(&state, &bearer, "alpha").await;
    let (status, _) = publish_ready_v3(&state, &bearer, &word).await;
    assert_eq!(status, StatusCode::CREATED);
    let draft = sentence(&state, &bearer, &word).await;
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let mut revoke = pool.begin().await.unwrap();
    sqlx::query("UPDATE admins SET permission_version=permission_version+1 WHERE id=$1")
        .bind(actor)
        .execute(&mut *revoke)
        .await
        .unwrap();
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='sentences.publish'")
        .bind(actor).execute(&mut *revoke).await.unwrap();
    let request_state = state.clone();
    let request = tokio::spawn(async move {
        call(
            &request_state,
            Method::POST,
            &format!(
                "{ROOT}/sentences/{}/publications",
                draft["id"].as_str().unwrap()
            ),
            &bearer,
            Some(Uuid::now_v7()),
            Some(revisions(&draft)),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%FROM admins%FOR SHARE%')")
                .fetch_one(&pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("publisher must wait on the authorization row");
    revoke.commit().await.unwrap();
    let (status, response) = request.await.unwrap();
    assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentence_publications")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn batch4_mixed_replaces_only_selected_publication_sources(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let mut a = batch3_word(&state, &bearer, "alpha").await;
    let mut meanings = writable_v3_meanings(&a);
    let mut extra = meanings["pos"][0]["senses"][0].clone();
    extra["id"] = json!(Uuid::now_v7());
    for definition in extra["definitions"].as_array_mut().unwrap() {
        definition["id"] = json!(Uuid::now_v7());
        definition["content_id"] = json!(Uuid::now_v7());
    }
    meanings["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .push(extra);
    a = save_v3_meanings(&state, &bearer, &a, meanings).await;
    let b = batch3_word(&state, &bearer, "beta").await;
    for word in [&a, &b] {
        let (status, response) = publish_ready_v3(&state, &bearer, word).await;
        assert_eq!(status, StatusCode::CREATED, "{response}");
    }
    let draft = sentence(&state, &bearer, &a).await;
    let published = publish_sentence(&state, &bearer, &draft).await;
    let other_draft = sentence(&state, &bearer, &a).await;
    let b_sentence = sentence(&state, &bearer, &b).await;
    let mut repaired = b_sentence["content"].clone();
    repaired["sentence"]["id"] = draft["id"].clone();
    let (status, repaired) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/sentences/{}", draft["id"].as_str().unwrap()),
        &bearer,
        None,
        Some(json!({"base_revision":published["revision"],"content":repaired})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repaired}");
    let mut meanings = writable_v3_meanings(&a);
    meanings["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    a = save_v3_meanings(&state, &bearer, &a, meanings).await;
    let mut input = batch3_input(&[&a]);
    input["sentences"] = json!([item(&repaired)]);
    let path = format!("{ROOT}/entries/publications/batch");
    let (status, response) = call(
        &state,
        Method::POST,
        &path,
        &bearer,
        Some(Uuid::now_v7()),
        Some(input.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "unselected draft must still protect its target: {response}"
    );
    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/sentences/{}", other_draft["id"].as_str().unwrap()),
        &bearer,
        None,
        Some(json!({"base_revision":other_draft["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{response}");
    let (status, response) = publish_ready_v3(&state, &bearer, &a).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "old sentence publication still protects its target: {response}"
    );
    let (status, response) = call(
        &state,
        Method::POST,
        &path,
        &bearer,
        Some(Uuid::now_v7()),
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
}

async fn sentence(state: &AppState, bearer: &str, word: &Value) -> Value {
    let pos = &word["word"]["forms"]["pos"][0];
    let form = &pos["forms"][0];
    let variants = &form["regional_variants"];
    let variant = if variants["mode"] == "common" {
        &variants["common"]
    } else {
        &variants["uk"]
    };
    let surface = variant["spelling"].as_str().unwrap();
    let sense = &word["word"]["meanings"]["pos"][0]["senses"][0]["id"];
    let translation = Uuid::now_v7();
    let input = json!({"source_entry_id":word["word"]["id"],"source_sense_id":sense,"content":{
        "sentence":{"id":Uuid::now_v7(),"level":"B1","en_text":{"mode":"unified","common":{"id":Uuid::now_v7(),"origin":"manual","value":{"version":2,"text":surface,"annotations":[]}}},"zh_text_id":translation,"zh_text":{"version":2,"text":"测试。","annotations":[]},"zh_translations":[{"id":translation,"band":"balanced_fluency","language":"zh","content":{"version":2,"text":"测试。","annotations":[]}}],"links":[]},
        "annotations":[{"id":Uuid::now_v7(),"source_dialect":"common","source_segments":[{"start":0,"end":surface.chars().count(),"surface":surface}],"target":{"state":"linked","target_entry_id":word["word"]["id"],"target_pos_id":pos["pos_id"],"target_base_form_id":form["id"],"target_form_id":form["id"],"target_variant_id":variant["id"],"target_sense_id":sense}}]
    }});
    let (status, value) = call(
        state,
        Method::POST,
        &format!("{ROOT}/sentences"),
        bearer,
        None,
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

fn revisions(sentence: &Value) -> Value {
    json!({"base_revision":sentence["revision"],"base_lifecycle_revision":sentence["lifecycle_revision"]})
}

fn item(sentence: &Value) -> Value {
    json!({"sentence_id":sentence["id"],"base_revision":sentence["revision"],"base_lifecycle_revision":sentence["lifecycle_revision"]})
}

async fn publish_sentence(state: &AppState, bearer: &str, value: &Value) -> Value {
    let (status, result) = call(
        state,
        Method::POST,
        &format!(
            "{ROOT}/sentences/{}/publications",
            value["id"].as_str().unwrap()
        ),
        bearer,
        Some(Uuid::now_v7()),
        Some(revisions(value)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result
}

async fn edit_level(state: &AppState, bearer: &str, value: &Value, level: &str) -> Value {
    let mut content = value["content"].clone();
    content["sentence"]["level"] = json!(level);
    let (status, result) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/sentences/{}", value["id"].as_str().unwrap()),
        bearer,
        None,
        Some(json!({"base_revision":value["revision"],"content":content})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result
}

async fn public_list(state: &AppState, bearer: &str, entry: Option<&Value>) -> Value {
    let query = entry.map_or_else(String::new, |id| {
        format!("?entry_id={}", id.as_str().unwrap())
    });
    let (status, value) = call(
        state,
        Method::GET,
        &format!("{ROOT}/sentences{query}"),
        bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

#[sqlx::test]
async fn batch4_sentence_publication_isolated_history_and_idempotency(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin_with_role(&pool, AdminRole::Admin).await;
    let bearer = token(&state, actor);
    let word = batch3_word(&state, &bearer, "alpha").await;
    let (status, _) = publish_ready_v3(&state, &bearer, &word).await;
    assert_eq!(status, StatusCode::CREATED);
    let created = sentence(&state, &bearer, &word).await;
    assert_eq!(public_list(&state, &bearer, None).await["total"], 0);
    let path = format!("{ROOT}/sentences/{}", created["id"].as_str().unwrap());
    let (status, _) = call(&state, Method::GET, &path, &bearer, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let key = Uuid::now_v7();
    let input = revisions(&created);
    let (status, published) = call(
        &state,
        Method::POST,
        &format!("{path}/publications"),
        &bearer,
        Some(key),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    let (status, replay) = call(
        &state,
        Method::POST,
        &format!("{path}/publications"),
        &bearer,
        Some(key),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay, published);
    let first = published["current_publication_id"].clone();
    assert_eq!(
        public_list(&state, &bearer, Some(&word["word"]["id"])).await["total"],
        1
    );
    let edited = edit_level(&state, &bearer, &published, "C1").await;
    assert_eq!(
        public_list(&state, &bearer, None).await["items"][0]["content"]["sentence"]["level"],
        "B1"
    );
    let second = publish_sentence(&state, &bearer, &edited).await;
    assert_eq!(
        public_list(&state, &bearer, None).await["items"][0]["content"]["sentence"]["level"],
        "C1"
    );
    let (status, rolled) = call(
        &state,
        Method::POST,
        &format!("{path}/publications/{}/rollback", first.as_str().unwrap()),
        &bearer,
        Some(Uuid::now_v7()),
        Some(revisions(&second)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rolled}");
    assert_ne!(rolled["current_publication_id"], first);
    assert_eq!(rolled["content"], edited["content"]);
    assert_eq!(rolled["revision"], edited["revision"]);
    assert_eq!(
        public_list(&state, &bearer, None).await["items"][0]["content"]["sentence"]["level"],
        "B1"
    );
    let (_, history) = call(
        &state,
        Method::GET,
        &format!("{path}/publications"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(history.as_array().unwrap().len(), 3);
    assert_eq!(history[0]["rollback_of_publication_id"], first);
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='sentences.publish'")
        .bind(actor).execute(&pool).await.unwrap();
    let (status, _) = call(
        &state,
        Method::POST,
        &format!("{path}/publications"),
        &bearer,
        Some(key),
        Some(input),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "revoked permission must precede replay"
    );
    let result = sqlx::raw_sql(include_str!(
        "../../migrations/20260923030000_shared_sentence_publications.down.sql"
    ))
    .execute(&pool)
    .await;
    assert!(result.is_err());
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentence_publications")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 3);
}

#[sqlx::test]
async fn batch4_withdrawal_requires_owner_confirmation_and_explicit_restore(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let other = seed_admin_with_role(&pool, AdminRole::Admin).await;
    let other_bearer = token(&state, other);
    let word = batch3_word(&state, &bearer, "alpha").await;
    let (status, _) = publish_ready_v3(&state, &bearer, &word).await;
    assert_eq!(status, StatusCode::CREATED);
    let draft = sentence(&state, &bearer, &word).await;
    let published = publish_sentence(&state, &bearer, &draft).await;
    let path = format!("{ROOT}/sentences/{}", draft["id"].as_str().unwrap());
    let (_, impact) = call(
        &state,
        Method::GET,
        &format!("{path}/withdrawal-impact"),
        &bearer,
        None,
        None,
    )
    .await;
    let input = json!({"base_revision":published["revision"],"base_lifecycle_revision":published["lifecycle_revision"],"reason":"例句需要核查","impact_fingerprint":impact["fingerprint"]});
    let (status, _) = call(
        &state,
        Method::POST,
        &format!("{path}/withdraw"),
        &other_bearer,
        Some(Uuid::now_v7()),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let mut invalid = input.clone();
    invalid["reason"] = json!(" ");
    let (status, _) = call(
        &state,
        Method::POST,
        &format!("{path}/withdraw"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(invalid),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut stale = input.clone();
    stale["impact_fingerprint"] = json!("outdated");
    let (status, _) = call(
        &state,
        Method::POST,
        &format!("{path}/withdraw"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(stale),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, withdrawn) = call(
        &state,
        Method::POST,
        &format!("{path}/withdraw"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{withdrawn}");
    assert!(withdrawn["withdrawn_at"].is_string());
    assert_eq!(public_list(&state, &bearer, None).await["total"], 0);
    let edited = edit_level(&state, &bearer, &withdrawn, "C1").await;
    let republished = publish_sentence(&state, &bearer, &edited).await;
    assert!(republished["withdrawn_at"].is_string());
    assert_eq!(public_list(&state, &bearer, None).await["total"], 0);
    let (status, rolled) = call(
        &state,
        Method::POST,
        &format!(
            "{path}/publications/{}/rollback",
            published["current_publication_id"].as_str().unwrap()
        ),
        &bearer,
        Some(Uuid::now_v7()),
        Some(revisions(&republished)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rolled}");
    assert!(rolled["withdrawn_at"].is_string());
    assert_eq!(public_list(&state, &bearer, None).await["total"], 0);
    let (status, restored) = call(
        &state,
        Method::POST,
        &format!("{path}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(revisions(&rolled)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert!(restored["withdrawn_at"].is_null());
    assert_eq!(public_list(&state, &bearer, None).await["total"], 1);
    assert_eq!(restored["content"], edited["content"]);
    let (status, _) = call(
        &state,
        Method::DELETE,
        &path,
        &bearer,
        None,
        Some(json!({"base_revision":restored["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentence_publication_annotations")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 3);
}

#[sqlx::test]
async fn batch4_local_visibility_follows_host_publication_not_sentence_updates(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let word = batch3_word(&state, &bearer, "alpha").await;
    let word_id = word["word"]["id"].as_str().unwrap();
    let (status, _) = publish_ready_v3(&state, &bearer, &word).await;
    assert_eq!(status, StatusCode::CREATED);
    let first: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id=$1")
            .bind(Uuid::parse_str(word_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    let draft = sentence(&state, &bearer, &word).await;
    let published = publish_sentence(&state, &bearer, &draft).await;
    let visibility_path = format!(
        "{ROOT}/entries/{word_id}/sentences/{}/visibility",
        draft["id"].as_str().unwrap()
    );
    let visibility = json!({"base_revision":word["word"]["revision"],"sense_id":word["word"]["meanings"]["pos"][0]["senses"][0]["id"],"hidden":true});
    let (status, hidden) = call(
        &state,
        Method::PUT,
        &visibility_path,
        &bearer,
        None,
        Some(visibility.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hidden}");
    assert_eq!(
        public_list(&state, &bearer, Some(&word["word"]["id"])).await["total"],
        1
    );
    let (_, draft_view) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/sentences?view=draft&entry_id={word_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(draft_view["total"], 0);
    let (_, word_hidden) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{word_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    let (status, _) = publish_ready_v3(&state, &bearer, &word_hidden).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        public_list(&state, &bearer, Some(&word["word"]["id"])).await["total"],
        0
    );
    let changed = edit_level(&state, &bearer, &published, "C1").await;
    let _ = publish_sentence(&state, &bearer, &changed).await;
    assert_eq!(
        public_list(&state, &bearer, Some(&word["word"]["id"])).await["total"],
        0
    );
    let (_, current) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{word_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    let input = json!({"schema_version":3,"base_revision":current["word"]["revision"],"base_lifecycle_revision":current["word"]["lifecycle_revision"]});
    let (status, rolled) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{word_id}/publications/{first}/rollback"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{rolled}");
    assert_eq!(
        public_list(&state, &bearer, Some(&word["word"]["id"])).await["total"],
        1
    );
    let hidden_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentence_draft_hides")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(hidden_count, 1, "host rollback must preserve draft hiding");
    let refs: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentence_annotations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(refs, 1);
}

#[sqlx::test]
async fn batch4_mixed_publication_resolves_explicit_dependency_and_is_atomic(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let word = batch3_word(&state, &bearer, "alpha").await;
    let draft = sentence(&state, &bearer, &word).await;
    let path = format!(
        "{ROOT}/sentences/{}/publications",
        draft["id"].as_str().unwrap()
    );
    let (status, _) = call(
        &state,
        Method::POST,
        &path,
        &bearer,
        Some(Uuid::now_v7()),
        Some(revisions(&draft)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut input = batch3_input(&[&word]);
    input["sentences"] = json!([item(&draft)]);
    let key = Uuid::now_v7();
    let (status, result) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/publications/batch"),
        &bearer,
        Some(key),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{result}");
    assert_eq!(result["sentences"].as_array().unwrap().len(), 1);
    let (status, replay) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/publications/batch"),
        &bearer,
        Some(key),
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(result, replay);
    let current: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id=$1")
            .bind(Uuid::parse_str(word["word"]["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    let list = public_list(&state, &bearer, None).await;
    assert_eq!(list["total"], 1);
    assert_eq!(
        list["items"][0]["content"]["annotations"][0]["target"]["target_publication_id"],
        json!(current)
    );
}

#[sqlx::test]
async fn batch4_mixed_late_failure_rolls_back_word_and_sentence(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let word = batch3_word(&state, &bearer, "alpha").await;
    let draft = sentence(&state, &bearer, &word).await;
    sqlx::raw_sql("CREATE FUNCTION lexicon.fail_batch4_annotation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'batch4 late failure'; END $$; CREATE TRIGGER fail_batch4_annotation BEFORE INSERT ON lexicon.shared_sentence_publication_annotations FOR EACH ROW EXECUTE FUNCTION lexicon.fail_batch4_annotation();").execute(&pool).await.unwrap();
    let mut input = batch3_input(&[&word]);
    input["sentences"] = json!([item(&draft)]);
    let (status, _) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/publications/batch"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    for query in [
        "SELECT count(*) FROM lexicon.entry_publications",
        "SELECT count(*) FROM lexicon.shared_sentence_publications",
        "SELECT count(*) FROM lexicon.entries WHERE current_publication_id IS NOT NULL",
        "SELECT count(*) FROM lexicon.shared_sentences WHERE current_publication_id IS NOT NULL",
    ] {
        let count: i64 = sqlx::query_scalar(query).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0, "{query}");
    }
}

#[sqlx::test]
async fn batch4_mixed_reverse_order_concurrency_has_one_winner(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    let a = batch3_word(&state, &bearer, "alpha").await;
    let b = batch3_word(&state, &bearer, "beta").await;
    let sa = sentence(&state, &bearer, &a).await;
    let sb = sentence(&state, &bearer, &b).await;
    let mut x = batch3_input(&[&a, &b]);
    x["sentences"] = json!([item(&sa), item(&sb)]);
    let mut y = batch3_input(&[&b, &a]);
    y["sentences"] = json!([item(&sb), item(&sa)]);
    let path = format!("{ROOT}/entries/publications/batch");
    let (x, y) = tokio::join!(
        call(
            &state,
            Method::POST,
            &path,
            &bearer,
            Some(Uuid::now_v7()),
            Some(x)
        ),
        call(
            &state,
            Method::POST,
            &path,
            &bearer,
            Some(Uuid::now_v7()),
            Some(y)
        )
    );
    let mut statuses = [x.0, y.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::CREATED, StatusCode::CONFLICT],
        "{x:?} {y:?}"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.shared_sentence_publications")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 2);
}

#[sqlx::test]
async fn permission_mixed_batch_requires_only_present_resource_actions(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let actor = seed_admin_with_role(&pool, AdminRole::Admin).await;
    let bearer = token(&state, actor);
    let word = batch3_word(&state, &bearer, "alpha").await;
    let draft = sentence(&state, &bearer, &word).await;
    let path = format!("{ROOT}/entries/publications/batch");
    let mut mixed = batch3_input(&[&word]);
    mixed["sentences"] = json!([item(&draft)]);
    for missing in ["sentences.publish", "words.publish"] {
        sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key=$2")
            .bind(actor)
            .bind(missing)
            .execute(&pool)
            .await
            .unwrap();
        let (status, response) = call(
            &state,
            Method::POST,
            &path,
            &bearer,
            Some(Uuid::now_v7()),
            Some(mixed.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{missing}: {response}");
        let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM lexicon.entry_publications), (SELECT count(*) FROM lexicon.shared_sentence_publications)")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(counts, (0, 0), "缺任何非空资源的发布动作都须整批无副作用");
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,$2,$1)")
            .bind(actor).bind(missing).execute(&pool).await.unwrap();
    }
    let (status, result) = call(
        &state,
        Method::POST,
        &path,
        &bearer,
        Some(Uuid::now_v7()),
        Some(mixed),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{result}");
    sqlx::query(
        "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='words.publish'",
    )
    .bind(actor)
    .execute(&pool)
    .await
    .unwrap();
    let (_, current_sentence) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/sentences/{}", draft["id"].as_str().unwrap()),
        &bearer,
        None,
        None,
    )
    .await;
    let (status, result) = call(
        &state,
        Method::POST,
        &path,
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({"schema_version":3,"items":[],"sentences":[item(&current_sentence)]})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "空词条集合不能要求words.publish：{result}"
    );
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='sentences.publish'")
        .bind(actor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.publish',$1)")
        .bind(actor).execute(&pool).await.unwrap();
    let next_word = batch3_word(&state, &bearer, "beta").await;
    let (status, result) = call(
        &state,
        Method::POST,
        &path,
        &bearer,
        Some(Uuid::now_v7()),
        Some(batch3_input(&[&next_word])),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "空例句集合不能要求sentences.publish：{result}"
    );
}

fn permission_service(state: &AppState) -> tsz_rust::lexicon::service::LexiconService {
    use tsz_rust::lexicon::{
        detection_store::DetectionStore, impact_store::ImpactStore, repository::LexiconRepository,
        service::LexiconService, surface_policy::SurfacePolicyStore,
        surface_snapshot::SurfaceSnapshotStore,
    };
    LexiconService::new(
        LexiconRepository::new(state.pool.clone()),
        DetectionStore::new(state.redis.clone()),
        ImpactStore::new(state.redis.clone()),
        SurfaceSnapshotStore::new(
            state.redis.clone(),
            std::time::Duration::from_secs(600),
            std::time::Duration::from_secs(600),
        ),
        SurfacePolicyStore::new(state.redis.clone()),
        std::sync::Arc::from(&b"permission-test-key"[..]),
    )
}

#[sqlx::test]
async fn permission_edit_scope_applies_to_drafts_and_published_revisions_at_service_boundary(
    pool: PgPool,
) {
    use tsz_rust::lexicon::{
        dto::{DeleteDraftInput, EntryLifecycleInput, SaveMeaningsStepInputV3},
        service::LexiconServiceError,
    };
    let state = batch3_state(&pool).await;
    let owner = seed_admin(&pool).await;
    let outsider = seed_admin_with_role(&pool, AdminRole::Admin).await;
    let bearer = token(&state, owner);
    let service = permission_service(&state);
    for published in [false, true] {
        let mut word = batch3_word(&state, &bearer, if published { "beta" } else { "alpha" }).await;
        if published {
            let (status, response) = publish_ready_v3(&state, &bearer, &word).await;
            assert_eq!(status, StatusCode::CREATED, "{response}");
            word = response;
        }
        let id = Uuid::parse_str(word["word"]["id"].as_str().unwrap()).unwrap();
        let input: SaveMeaningsStepInputV3 = serde_json::from_value(json!({"schema_version":3,"base_revision":word["word"]["revision"],"intent":"save","content":writable_v3_meanings(&word)})).unwrap();
        let error = service
            .save_meanings_v3(outsider, Uuid::now_v7(), id, input.clone(), true)
            .await
            .unwrap_err();
        assert!(
            matches!(error, LexiconServiceError::EntryEditForbidden),
            "客户端超管bool不可扩权：{error:?}"
        );
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.edit_others',$1)")
            .bind(outsider).execute(&pool).await.unwrap();
        let edited = service
            .save_meanings_v3(outsider, Uuid::now_v7(), id, input, false)
            .await
            .unwrap();
        let archived = service
            .archive(
                outsider,
                Uuid::now_v7(),
                id,
                Uuid::now_v7(),
                EntryLifecycleInput {
                    base_revision: edited.word.revision,
                    base_lifecycle_revision: edited.word.lifecycle_revision,
                    confirmed_surface_match_token: None,
                },
                true,
                true,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(archived, LexiconServiceError::EntryPublishForbidden),
            "编辑他人不授予他人生命周期权限：{archived:?}"
        );
        let deletion = service
            .delete_draft(
                outsider,
                Uuid::now_v7(),
                id,
                DeleteDraftInput {
                    base_revision: edited.word.revision,
                    base_lifecycle_revision: edited.word.lifecycle_revision,
                },
                true,
                true,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(deletion, LexiconServiceError::EntryDeleteForbidden),
            "范围权限/伪造bool不是超管"
        );
        sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='words.edit_others'")
            .bind(outsider).execute(&pool).await.unwrap();
        let input: SaveMeaningsStepInputV3 = serde_json::from_value(json!({"schema_version":3,"base_revision":edited.word.revision,"intent":"save","content":writable_v3_meanings(&serde_json::to_value(&edited).unwrap())})).unwrap();
        assert!(matches!(
            service
                .save_meanings_v3(outsider, Uuid::now_v7(), id, input, true)
                .await,
            Err(LexiconServiceError::EntryEditForbidden)
        ));
    }
}

#[sqlx::test]
async fn permission_create_annotation_updates_require_edit_scope_without_super_admin(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let owner = seed_admin(&pool).await;
    let editor = seed_admin_with_role(&pool, AdminRole::Admin).await;
    let existing = create_v3_with_complete_forms(&state, &pool, &token(&state, owner)).await;
    let bearer = token(&state, editor);
    let mut body = entry_annotations_create_body(
        &state,
        &bearer,
        "harbour",
        json!({"mode":"distinguish","uk":"harbour","us":"harbor","source_dialect":"uk"}),
    )
    .await;
    let key = Uuid::now_v7();
    let (status, required) = entry_annotations_submit(&state, &bearer, key, &mut body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{required}");
    body["annotation"] = json!("new");
    body["annotation_updates"] = entry_annotations_updates(&required, &["old"]);
    let (status, denied) = entry_annotations_submit(&state, &bearer, key, &mut body).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    assert_eq!(denied["code"], "entry_annotation_forbidden");
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.edit_others',$1)")
        .bind(editor).execute(&pool).await.unwrap();
    let (status, created) = entry_annotations_submit(&state, &bearer, key, &mut body).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "有create、edit及编辑他人范围的普通管理员可提交嵌套标注：{created}"
    );
    let old_label: String =
        sqlx::query_scalar("SELECT annotation FROM lexicon.entries WHERE id=$1")
            .bind(Uuid::parse_str(existing["word"]["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(old_label, "old");
    let role: String = sqlx::query_scalar("SELECT role FROM admins WHERE id=$1")
        .bind(editor)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(role, "admin");
}

#[sqlx::test]
async fn permission_shared_sentence_body_scope_and_host_scope_are_independent(pool: PgPool) {
    let state = batch3_state(&pool).await;
    let owner = seed_admin(&pool).await;
    let outsider = seed_admin_with_role(&pool, AdminRole::Admin).await;
    let bearer = token(&state, owner);
    let other_bearer = token(&state, outsider);
    let word = batch3_word(&state, &bearer, "alpha").await;
    let word_id = word["word"]["id"].as_str().unwrap();
    let (status, _) = publish_ready_v3(&state, &bearer, &word).await;
    assert_eq!(status, StatusCode::CREATED);
    for published in [false, true] {
        let mut current = sentence(&state, &bearer, &word).await;
        if published {
            current = publish_sentence(&state, &bearer, &current).await;
        }
        let path = format!("{ROOT}/sentences/{}", current["id"].as_str().unwrap());
        let body = json!({"base_revision":current["revision"],"content":current["content"]});
        let (status, denied) = call(
            &state,
            Method::PUT,
            &path,
            &other_bearer,
            None,
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "已发布与草稿都须例句归属：{denied}"
        );
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.edit_others',$1)")
            .bind(outsider).execute(&pool).await.unwrap();
        let (status, denied) = call(
            &state,
            Method::PUT,
            &path,
            &other_bearer,
            None,
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "词条范围不能替代例句范围：{denied}"
        );
        sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='words.edit_others'")
            .bind(outsider).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'sentences.edit_others',$1)")
            .bind(outsider).execute(&pool).await.unwrap();
        let (status, edited) =
            call(&state, Method::PUT, &path, &other_bearer, None, Some(body)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "纯例句编辑无需编辑被引词条：{edited}"
        );
        let context_body = json!({"base_revision":edited["revision"],"content":edited["content"],"context_entry_id":word_id,"context_sense_id":word["word"]["meanings"]["pos"][0]["senses"][0]["id"]});
        let (status, denied) = call(
            &state,
            Method::PUT,
            &path,
            &other_bearer,
            None,
            Some(context_body.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "带宿主上下文另查词条归属：{denied}"
        );
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.edit_others',$1)")
            .bind(outsider).execute(&pool).await.unwrap();
        let (status, edited) = call(
            &state,
            Method::PUT,
            &path,
            &other_bearer,
            None,
            Some(context_body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "两对象分别获授权后可编辑：{edited}");
        let (status, denied) = call(
            &state,
            Method::DELETE,
            &path,
            &other_bearer,
            None,
            Some(json!({"base_revision":edited["revision"]})),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "两种编辑范围不构成永久删除超管身份：{denied}"
        );
        sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key=ANY(ARRAY['words.edit_others','sentences.edit_others'])")
            .bind(outsider).execute(&pool).await.unwrap();
    }
}
