use super::*;

async fn component_request(
    state: &AppState,
    actor: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(format!("{ROOT}{path}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "Bearer {}",
                state.admin_token_manager.generate(actor, "admin").unwrap()
            ),
        )
        .header("x-tsz-next-forms", "v1")
        .header("x-tsz-next-component-usages", "v1")
        .header("x-tsz-next-component-match", "v1")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = tsz_rust::router(state.clone()).oneshot(req).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[sqlx::test]
async fn phrase_components_roundtrip_and_legacy_save_keeps_links(pool: PgPool) {
    let actor = admin(
        &pool,
        &[
            "words.access",
            "words.create",
            "words.edit",
            "words.publish",
        ],
    )
    .await;
    sqlx::query(
        "INSERT INTO catalog.parts_of_speech(id, kind, code, name_zh, name_en, short_name_zh, abbreviation, full_name_en)
        VALUES($1, 'phrase', 'phrase_noun', '名词性成分', 'Nominal constituent', '名词性成分', 'np.', 'nominal constituent')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
    let reader = admin(&pool, &["words.access"]).await;
    let state = AppState::for_test(pool);
    let word = create(&state, actor).await;
    let word_path = format!("/entries/{}", word["id"].as_str().unwrap());
    let mut word_draft = word["draft"].clone();
    let word_sense = Uuid::now_v7();
    let forms = form_editor();
    word_draft["form_editor"] = forms.clone();
    word_draft["senses"] = json!([{
        "id":word_sense,"part_of_speech":"noun","definitions":[
            {"id":Uuid::now_v7(),"locale":"zh-Hans","style":"definition","text":"中心"}
        ],"equivalents":[]
    }]);
    let (status, saved) = call(
        &state,
        Some(actor),
        "PUT",
        &word_path,
        json!({"expected_revision":1,"draft":word_draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let published_word = saved["draft"].clone();

    let (status, phrase) = call(
        &state,
        Some(actor),
        "POST",
        "/entries",
        json!({"target_language":"en","kind":"phrase","headword":"centre stage"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{phrase}");
    let (status, matches) = component_request(
        &state,
        reader,
        "POST",
        "/entry-matches",
        json!({"target_language":"en","medium_locale":"zh-Hans","surface":"centre","page":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{matches}");
    assert_eq!(matches["items"][0]["id"], word["id"]);
    let (status, _) = call(
        &state,
        Some(reader),
        "POST",
        "/entry-matches",
        json!({"target_language":"en","medium_locale":"zh-Hans","surface":"centre"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let phrase_path = format!("/entries/{}", phrase["id"].as_str().unwrap());
    let mut draft = phrase["draft"].clone();
    let sense = Uuid::now_v7();
    let second_sense = Uuid::now_v7();
    let binding = &forms["pos"][0]["forms"][0];
    let usages = json!([{
        "target_source":"lexicon_next",
        "usage":{
            "state":"resolved","id":Uuid::now_v7(),"literal":"centre",
            "target_word_id":word["id"],"target_pos_id":forms["pos"][0]["pos_id"],
            "target_base_form_id":binding["id"],"target_form_id":binding["id"],
            "target_variant_id":binding["regional_bindings"]["uk"]["id"],
            "target_sense_id":word_sense,"target_dialect":"uk","target_form_type":"base",
            "target_headword":"centre","target_gloss":"中心"
        }
    }]);
    let mut invalid = usages.clone();
    invalid[0]["usage"]["target_sense_id"] = json!(Uuid::now_v7());
    draft["senses"] = json!([
        {"id":sense,"part_of_speech":"phrase_noun","definitions":[],"equivalents":[],"component_usages":invalid},
        {"id":second_sense,"part_of_speech":"phrase_noun","definitions":[],"equivalents":[]}
    ]);
    let (status, _) = component_request(
        &state,
        actor,
        "PUT",
        &phrase_path,
        json!({"expected_revision":1,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    draft["senses"][0]["component_usages"] = usages.clone();
    let (status, saved) = component_request(
        &state,
        actor,
        "PUT",
        &phrase_path,
        json!({"expected_revision":1,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["draft"]["senses"][0]["component_usages"], usages);
    assert!(
        saved["draft"]["senses"][1]
            .get("component_usages")
            .is_none()
    );

    let (_, mut legacy) = call(&state, Some(actor), "GET", &phrase_path, Value::Null).await;
    assert!(
        legacy["draft"]["senses"][0]
            .get("component_usages")
            .is_none()
    );
    legacy["draft"]["senses"][0]["definitions"] = json!([
        {"id":Uuid::now_v7(),"locale":"zh-Hans","style":"definition","text":"中心位置"}
    ]);
    let (status, saved) = call(
        &state,
        Some(actor),
        "PUT",
        &phrase_path,
        json!({"expected_revision":2,"draft":legacy["draft"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (status, mut reopened) =
        component_request(&state, actor, "GET", &phrase_path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    assert_eq!(reopened["draft"]["senses"][0]["component_usages"], usages);
    let (status, view) = component_request(
        &state,
        actor,
        "GET",
        &format!("{phrase_path}/view?target_locale=en-GB&medium_locale=zh-Hans"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["senses"][0]["component_usages"], usages);

    let (status, preview) = component_request(
        &state,
        actor,
        "POST",
        &format!("{phrase_path}/publication-preview"),
        json!({"base_revision":3,"base_lifecycle_revision":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert!(
        preview["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["node_id"] == json!(sense)
                && issue["field"] == "component_usages"
                && issue["message"].as_str().unwrap().contains("尚未发布")),
        "{preview}"
    );

    sqlx::query("UPDATE admins SET role='super_admin' WHERE id=$1")
        .bind(actor)
        .execute(&state.pool)
        .await
        .unwrap();
    let (status, blocked) = component_request(
        &state,
        actor,
        "POST",
        "/workspace/commands",
        json!({"request_id":Uuid::now_v7(),"action":"archive",
            "items":[{"entry_id":word["id"],"base_revision":2,"base_lifecycle_revision":1}]}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
    assert!(
        blocked["detail"].as_str().unwrap().contains("成分用词"),
        "{blocked}"
    );

    // A published word can have a newer draft with the same node IDs and new text.
    let word_id: Uuid = serde_json::from_value(word["id"].clone()).unwrap();
    sqlx::query(
        "INSERT INTO lexicon_next.publications(id,entry_id,revision,lifecycle_revision,published_by_admin_id,snapshot)
         VALUES($1,$2,2,2,$3,$4)",
    )
    .bind(Uuid::now_v7())
    .bind(word_id)
    .bind(actor)
    .bind(json!({"draft":published_word,"sentence_resources":[],"relation_targets":[]}))
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE lexicon_next.entries SET status='published', lifecycle_revision=2 WHERE id=$1",
    )
    .bind(word_id)
    .execute(&state.pool)
    .await
    .unwrap();
    let mut newer_word = published_word;
    newer_word["senses"][0]["definitions"][0]["text"] = json!("圆心");
    let (status, saved) = call(
        &state,
        Some(actor),
        "PUT",
        &word_path,
        json!({"expected_revision":2,"draft":newer_word}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let mut draft_usage = usages;
    draft_usage[0]["usage"]["target_gloss"] = json!("圆心");
    reopened["draft"]["senses"][0]["component_usages"] = draft_usage.clone();
    let (status, mut saved) = component_request(
        &state,
        actor,
        "PUT",
        &phrase_path,
        json!({"expected_revision":3,"draft":reopened["draft"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["draft"]["senses"][0]["component_usages"], draft_usage);

    saved["draft"]["senses"][0]["component_usages"] = json!([]);
    let (status, saved) = component_request(
        &state,
        actor,
        "PUT",
        &phrase_path,
        json!({"expected_revision":4,"draft":saved["draft"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, cleared) = component_request(&state, actor, "GET", &phrase_path, Value::Null).await;
    assert_eq!(cleared["draft"]["senses"][0]["component_usages"], json!([]));
    let (status, archived) = component_request(
        &state,
        actor,
        "POST",
        "/workspace/commands",
        json!({"request_id":Uuid::now_v7(),"action":"archive",
            "items":[{"entry_id":word["id"],"base_revision":3,"base_lifecycle_revision":2}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
}

#[sqlx::test]
async fn phrase_components_reject_new_v1_links_and_allow_clearing_existing_ones(pool: PgPool) {
    let actor = admin(
        &pool,
        &[
            "words.access",
            "words.create",
            "words.edit",
            "words.publish",
        ],
    )
    .await;
    let state = AppState::for_test(pool.clone());
    sqlx::query(
        "INSERT INTO catalog.parts_of_speech(id, kind, code, name_zh, name_en, short_name_zh, abbreviation, full_name_en)
        VALUES($1, 'phrase', 'phrase_noun', '名词性成分', 'Nominal constituent', '名词性成分', 'np.', 'nominal constituent')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();

    let word = Uuid::now_v7();
    let sense = Uuid::now_v7();
    let forms = form_editor();
    let pos = &forms["pos"][0];
    let form = &pos["forms"][0];
    sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,revision,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word',1,'{}',$2,$2)")
        .bind(word)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO lexicon.entry_editor_projection(entry_id,forms,meanings,rebuilt_revision) VALUES($1,$2,$3,1)")
        .bind(word)
        .bind(&forms)
        .bind(json!({"sense_groups":[],"pos":[{"pos_id":pos["pos_id"],"grammar_structures":[],"senses":[{
            "id":sense,"sub_pos":"N-COUNT","level":"A1","depends_on_context":false,
            "definitions":[{"id":Uuid::now_v7(),"content_id":Uuid::now_v7(),"level":"A1","definition_mode":"zh_definition",
                "content":{"version":2,"text":"中心","annotations":[]}}],"sentences":[],"relations":[]
        }]}]}))
        .execute(&pool)
        .await
        .unwrap();

    let (status, phrase) = call(
        &state,
        Some(actor),
        "POST",
        "/entries",
        json!({"target_language":"en","kind":"phrase","headword":"centre stage"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{phrase}");
    let phrase_id: Uuid = serde_json::from_value(phrase["id"].clone()).unwrap();
    let path = format!("/entries/{phrase_id}");
    let mut draft = phrase["draft"].clone();
    let phrase_sense = Uuid::now_v7();
    let usages = json!([{
        "target_source":"v3",
        "usage":{
            "state":"resolved","id":Uuid::now_v7(),"literal":"centre",
            "target_word_id":word,"target_pos_id":pos["pos_id"],
            "target_base_form_id":form["id"],"target_form_id":form["id"],
            "target_variant_id":form["regional_bindings"]["uk"]["id"],
            "target_sense_id":sense,"target_dialect":"uk","target_form_type":"base",
            "target_headword":"centre","target_gloss":"中心"
        }
    }]);
    draft["senses"] = json!([{
        "id":phrase_sense,"part_of_speech":"phrase_noun","definitions":[],"equivalents":[],"component_usages":usages
    }]);
    let (status, rejected) = component_request(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert!(rejected["detail"].as_str().unwrap().contains("新版词条"));

    // Simulate a draft saved before the same-catalog restriction.
    sqlx::query(
        "UPDATE lexicon_next.draft_revisions SET document=$2 WHERE entry_id=$1 AND revision=1",
    )
    .bind(phrase_id)
    .bind(&draft)
    .execute(&pool)
    .await
    .unwrap();
    let (status, reopened) = component_request(&state, actor, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    assert_eq!(reopened["draft"]["senses"][0]["component_usages"], usages);

    draft["senses"][0]["definitions"] = json!([
        {"id":Uuid::now_v7(),"locale":"zh-Hans","style":"definition","text":"中心位置"}
    ]);
    let (status, mut saved) = component_request(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":1,"draft":draft}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["draft"]["senses"][0]["component_usages"], usages);

    let (status, preview) = component_request(
        &state,
        actor,
        "POST",
        &format!("{path}/publication-preview"),
        json!({"base_revision":2,"base_lifecycle_revision":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert!(
        preview["issues"].as_array().unwrap().iter().any(|issue| {
            issue["node_id"] == json!(phrase_sense)
                && issue["field"] == "component_usages"
                && issue["message"].as_str().unwrap().contains("V1 关联")
        }),
        "{preview}"
    );

    saved["draft"]["senses"][0]["component_usages"] = json!([]);
    let (status, cleared) = component_request(
        &state,
        actor,
        "PUT",
        &path,
        json!({"expected_revision":2,"draft":saved["draft"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    let (_, reopened) = component_request(&state, actor, "GET", &path, Value::Null).await;
    assert_eq!(
        reopened["draft"]["senses"][0]["component_usages"],
        json!([])
    );
}
