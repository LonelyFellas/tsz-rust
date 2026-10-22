use super::*;

async fn shared_call(
    state: &AppState,
    bearer: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call_with_form_capability(
        state,
        method,
        path,
        bearer,
        None,
        body,
        (true, true, true, true),
        true,
    )
    .await
}

async fn save_shared(
    state: &AppState,
    bearer: &str,
    id: &str,
    revision: i64,
    content: Value,
) -> Value {
    save_shared_with_capabilities(
        state,
        bearer,
        id,
        revision,
        content,
        (true, true, true, true),
    )
    .await
}

async fn save_shared_with_capabilities(
    state: &AppState,
    bearer: &str,
    id: &str,
    revision: i64,
    content: Value,
    capabilities: (bool, bool, bool, bool),
) -> Value {
    let (status, impact) = call_with_form_capability(
        state,
        Method::POST,
        &format!("{ROOT}/entries/{id}/steps/forms/impact"),
        bearer,
        None,
        Some(json!({"schema_version":3,"base_revision":revision,"content":content})),
        capabilities,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    let mut body =
        json!({"schema_version":3,"base_revision":revision,"intent":"save","content":content});
    if let Some(token) = impact["confirmation_token"].as_str() {
        body["confirmed_impact_token"] = json!(token);
    }
    for (source, target) in [
        (
            "surface_confirmation_token",
            "confirmed_surface_match_token",
        ),
        ("impact_confirmation_token", "confirmed_impact_token"),
    ] {
        if let Some(token) = impact["surface_match_page"][source].as_str() {
            body[target] = json!(token);
        }
    }
    let (status, saved) = call_with_form_capability(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{id}/steps/forms"),
        bearer,
        None,
        Some(body),
        capabilities,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved
}

#[sqlx::test]
async fn shared_forms_omitted_components_preserve_but_explicit_empty_clears(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let (id, mut forms) = create_v3_phrase_draft(&state, &bearer, "shared component").await;
    let component = json!({"id":Uuid::now_v7(),"state":"unresolved","literal":"shared"});
    forms["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"] = json!([component]);
    let (_, saved) = save_v3_forms_after_impact(&state, &bearer, &id, 1, "save", forms).await;
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut content = read["word"]["forms"].clone();
    let path = "/pos/0/forms/0/regional_bindings/uk";
    content
        .pointer_mut(path)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("component_usages");
    let preserved = save_shared(
        &state,
        &bearer,
        &id,
        saved["word"]["revision"].as_i64().unwrap(),
        content,
    )
    .await;
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(
        fresh["word"]["forms"].pointer(path).unwrap()["component_usages"],
        json!([component])
    );
    let entry_id = Uuid::parse_str(&id).unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.v3_phrase_variant_component_usages WHERE entry_id=$1",
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let mut content = fresh["word"]["forms"].clone();
    content.pointer_mut(path).unwrap()["component_usages"] = json!([]);
    save_shared(
        &state,
        &bearer,
        &id,
        preserved["word"]["revision"].as_i64().unwrap(),
        content,
    )
    .await;
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(
        fresh["word"]["forms"].pointer(path).unwrap()["component_usages"],
        json!([])
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.v3_phrase_variant_component_usages WHERE entry_id=$1",
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn shared_forms_partial_capabilities_preserve_settings_across_binding_changes(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut content = read["word"]["forms"].clone();
    for side in ["uk", "us"] {
        let variant = &mut content["contents"][0]["regional_variants"][side];
        let speech = json!({"content":{"version":2,"text":variant["spelling"],"annotations":[]},"voice_profile":{"voices":[{"voice_id":"sonia","enabled":true,"rate_percent":10}]}});
        variant["spelling_rich"] = json!({"version":2,"text":variant["spelling"],"annotations":[{"type":"italic","start":0,"end":1}]});
        variant["spelling_speech"] = speech.clone();
        variant["pronunciations"][0]["synthesis"] = json!({"alphabet":"ipa","ipa":"","ups":"","uk":{"ipa":"kæt","ups":"K AE T","ipa_speech":speech,"ups_speech":speech}});
    }
    let saved = save_shared(
        &state,
        &bearer,
        id,
        legacy["word"]["revision"].as_i64().unwrap(),
        content,
    )
    .await;
    let expected = saved["word"]["forms"]["contents"].clone();
    let mut revision = saved["word"]["revision"].as_i64().unwrap();
    for (capabilities, binding_change) in [
        ((false, true, false, false), 0),
        ((true, true, false, true), 0),
        ((false, true, true, false), 0),
        ((false, true, false, false), 1),
        ((false, true, false, false), 2),
    ] {
        let (_, limited) = call_with_form_capability(
            &state,
            Method::GET,
            &format!("{ROOT}/entries/{id}"),
            &bearer,
            None,
            None,
            capabilities,
            true,
        )
        .await;
        let mut content = limited["word"]["forms"].clone();
        assert_eq!(
            content["contents"][0]["regional_variants"]["uk"]
                .get("spelling_rich")
                .is_some(),
            capabilities.0
        );
        assert_eq!(
            content["contents"][0]["regional_variants"]["uk"]
                .get("spelling_speech")
                .is_some(),
            capabilities.2
        );
        assert_eq!(content["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["synthesis"]["uk"].get("ipa_speech").is_some(), capabilities.3);
        if binding_change == 1 {
            add_verb_binding(&mut content);
        } else if binding_change == 2 {
            for pos in content["pos"].as_array_mut().unwrap() {
                let mut replacements = Vec::new();
                for form in pos["forms"].as_array_mut().unwrap() {
                    let previous = form["id"].clone();
                    form["id"] = json!(Uuid::now_v7());
                    replacements.push((previous, form["id"].clone()));
                    for side in ["uk", "us"] {
                        let variant = &mut form["regional_bindings"][side];
                        variant["id"] = json!(Uuid::now_v7());
                        for row in variant["pronunciations"].as_array_mut().unwrap() {
                            row["id"] = json!(Uuid::now_v7());
                        }
                    }
                }
                for group in pos["form_groups"].as_array_mut().unwrap() {
                    for member in group["members"].as_array_mut().unwrap() {
                        let replacement = replacements
                            .iter()
                            .find(|(old, _)| old == &member["form_id"])
                            .unwrap();
                        member["id"] = json!(Uuid::now_v7());
                        member["form_id"] = replacement.1.clone();
                    }
                }
            }
        }
        let saved =
            save_shared_with_capabilities(&state, &bearer, id, revision, content, capabilities)
                .await;
        revision = saved["word"]["revision"].as_i64().unwrap();
        let (_, fresh) = shared_call(
            &state,
            &bearer,
            Method::GET,
            &format!("{ROOT}/entries/{id}"),
            None,
        )
        .await;
        assert_eq!(fresh["word"]["forms"]["contents"], expected);
        for content in expected.as_array().unwrap() {
            let stored: Value = sqlx::query_scalar(
                "SELECT content FROM lexicon.v3_form_contents WHERE entry_id=$1 AND id=$2",
            )
            .bind(Uuid::parse_str(id).unwrap())
            .bind(Uuid::parse_str(content["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(&stored, content);
        }
    }
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut content = fresh["word"]["forms"].clone();
    for side in ["uk", "us"] {
        let variant = &mut content["contents"][0]["regional_variants"][side];
        variant["spelling_rich"]["annotations"] = json!([]);
        variant["spelling_speech"]["voice_profile"]["voices"] = json!([]);
        for alphabet in ["ipa_speech", "ups_speech"] {
            variant["pronunciations"][0]["synthesis"]["uk"][alphabet]["voice_profile"]["voices"] =
                json!([]);
        }
    }
    let expected = content["contents"].clone();
    save_shared(&state, &bearer, id, revision, content).await;
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["forms"]["contents"], expected);
}

fn add_verb_binding(content: &mut Value) -> Value {
    let mut verb = content["pos"][0].clone();
    verb["pos_id"] = json!(Uuid::now_v7());
    verb["pos"] = json!("verb");
    verb["forms"] = json!([verb["forms"][0].clone()]);
    verb["forms"][0]["id"] = json!(Uuid::now_v7());
    for side in ["common", "uk", "us"] {
        if let Some(variant) = verb["forms"][0]["regional_bindings"].get_mut(side) {
            variant["id"] = json!(Uuid::now_v7());
            variant["is_regular"] = json!(false);
            for row in variant["pronunciations"].as_array_mut().unwrap() {
                row["id"] = json!(Uuid::now_v7());
            }
        }
    }
    verb["form_groups"] = json!([{"id":Uuid::now_v7(),"is_regular":false,"scope":"general","members":[{"id":Uuid::now_v7(),"form_id":verb["forms"][0]["id"]}]}]);
    content["pos"].as_array_mut().unwrap().push(verb.clone());
    verb
}

#[sqlx::test]
async fn shared_forms_preserve_binding_identity_reject_old_writes_and_stale_revisions(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let entry_id = Uuid::parse_str(id).unwrap();
    let revision = legacy["word"]["revision"].as_i64().unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(read["word"]["capabilities"]["shared_form_contents"], true);
    assert!(
        read["word"]["capabilities"]
            .get("form_content_reassignment")
            .is_none()
    );
    for capability in [None, Some("v1"), Some("future")] {
        let mut request = Request::builder()
            .method(Method::GET)
            .uri(format!("{ROOT}/entries/{id}"))
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header("X-TSZ-Form-Contents", "v1");
        if let Some(value) = capability {
            request = request.header("X-TSZ-Form-Content-Reassignment", value);
        }
        let response = tsz_rust::router(state.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()[header::VARY]
                .to_str()
                .unwrap()
                .contains("x-tsz-form-content-reassignment")
        );
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        if capability == Some("v1") {
            assert_eq!(
                body["word"]["capabilities"]["form_content_reassignment"],
                true
            );
        } else {
            assert!(
                body["word"]["capabilities"]
                    .get("form_content_reassignment")
                    .is_none()
            );
        }
    }
    assert!(
        legacy["word"]["capabilities"]
            .get("form_content_reassignment")
            .is_none()
    );
    let mut content = read["word"]["forms"].clone();
    assert_eq!(
        content["contents"].as_array().unwrap().len(),
        2,
        "同拼写旧 form 不自动合并"
    );
    let verb = add_verb_binding(&mut content);
    content["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["actual_pron"] =
        json!("changed");
    let saved = save_shared(&state, &bearer, id, revision, content).await;
    let revision = saved["word"]["revision"].as_i64().unwrap();
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["forms"], saved["word"]["forms"]);
    let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM lexicon.v3_form_contents WHERE entry_id=$1),(SELECT count(*) FROM lexicon.v3_form_content_bindings WHERE entry_id=$1)").bind(entry_id).fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (2, 3));
    let variant_id = Uuid::parse_str(
        verb["forms"][0]["regional_bindings"]["uk"]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let parent: (Uuid, String, bool) = sqlx::query_as(
        "SELECT parent_node_id,node_role,stable_slot FROM lexicon.nodes WHERE id=$1",
    )
    .bind(variant_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        parent,
        (
            Uuid::parse_str(verb["forms"][0]["id"].as_str().unwrap()).unwrap(),
            "forms.form_variant:uk".into(),
            true
        )
    );
    let (_, old_read) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        old_read["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["pronunciations"]
            [0]["actual_pron"],
        "changed"
    );
    assert_eq!(
        old_read["word"]["forms"]["pos"][1]["forms"][0]["regional_variants"]["uk"]["pronunciations"]
            [0]["actual_pron"],
        "changed"
    );
    let old_body = json!({"schema_version":3,"base_revision":revision,"intent":"save","content":old_read["word"]["forms"]});
    for method in [Method::POST, Method::PUT] {
        let suffix = if method == Method::POST {
            "/impact"
        } else {
            ""
        };
        let mut body = old_body.clone();
        if method == Method::POST {
            body.as_object_mut().unwrap().remove("intent");
        }
        let (status, error) = call(
            &state,
            method,
            &format!("{ROOT}/entries/{id}/steps/forms{suffix}"),
            &bearer,
            None,
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["code"], "form_contents_client_upgrade_required");
    }
    let (status, stale) = shared_call(&state, &bearer, Method::PUT, &format!("{ROOT}/entries/{id}/steps/forms"), Some(json!({"schema_version":3,"base_revision":revision-1,"intent":"save","content":fresh["word"]["forms"]}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    let unchanged: i64 = sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id=$1")
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(unchanged, revision);

    let mut invalid = fresh["word"]["forms"].clone();
    invalid["pos"][1]["forms"][0]["content_id"] = json!(Uuid::now_v7());
    let (status, error) = shared_call(
        &state,
        &bearer,
        Method::POST,
        &format!("{ROOT}/entries/{id}/steps/forms/impact"),
        Some(json!({"schema_version":3,"base_revision":revision,"content":invalid})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    assert_eq!(
        error["field_issues"][0]["code"],
        "form_content_mapping_invalid"
    );
    assert_eq!(
        error["field_issues"][0]["node_location"]["form_id"],
        verb["forms"][0]["id"]
    );
    assert_eq!(
        error["field_issues"][0]["node_location"]["pos_id"],
        verb["pos_id"]
    );

    let mut mixed = fresh["word"]["forms"].clone();
    mixed["contents"][1]["dialect_rules"]["spelling_mode"] = json!("unified");
    mixed["contents"][1]["regional_variants"]["us"]["spelling"] =
        mixed["contents"][1]["regional_variants"]["uk"]["spelling"].clone();
    let mixed_saved = save_shared(&state, &bearer, id, revision, mixed).await;
    let revision = mixed_saved["word"]["revision"].as_i64().unwrap();
    let (status, error) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["code"], "form_contents_client_upgrade_required");
    let (status, error) = call(&state, Method::POST, &format!("{ROOT}/entries/{id}/archive"), &bearer, Some(Uuid::now_v7()), Some(json!({"base_revision":revision,"base_lifecycle_revision":mixed_saved["word"]["lifecycle_revision"]}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    let active: bool =
        sqlx::query_scalar("SELECT archived_at IS NULL FROM lexicon.entries WHERE id=$1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(active, "旧客户端失败不得先归档再在响应时拒绝");
}

fn reassign_form_content(forms: &mut Value, pos: usize, form: usize, target: usize) {
    let target = forms["contents"][target].clone();
    let binding = &mut forms["pos"][pos]["forms"][form];
    binding["content_id"] = target["id"].clone();
    for side in ["common", "uk", "us"] {
        if let Some(variant) = target["regional_variants"].get(side) {
            binding["regional_bindings"][side]["content_slot_id"] = variant["id"].clone();
            binding["regional_bindings"][side]["pronunciations"] = Value::Array(
                variant["pronunciations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| json!({"id":Uuid::now_v7(),"content_slot_id":row["id"]}))
                    .collect(),
            );
        }
    }
}

#[sqlx::test]
async fn shared_forms_reassignment_preserves_form_identity_and_round_trips(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let entry_id = Uuid::parse_str(id).unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut forms = read["word"]["forms"].clone();
    forms["pos"][0]["forms"][1]["form_type"] = json!("plural");
    add_verb_binding(&mut forms);
    let original = save_shared(
        &state,
        &bearer,
        id,
        legacy["word"]["revision"].as_i64().unwrap(),
        forms,
    )
    .await;
    let original_forms = &original["word"]["forms"];
    let original_form = &original_forms["pos"][0]["forms"][0];
    let mut revision = original["word"]["revision"].as_i64().unwrap();
    for target in [1, 0, 1] {
        let mut forms = original_forms.clone();
        if target != 0 {
            reassign_form_content(&mut forms, 0, 0, target);
        }
        let saved = save_shared(&state, &bearer, id, revision, forms.clone()).await;
        revision = saved["word"]["revision"].as_i64().unwrap();
        let (status, fresh) = shared_call(
            &state,
            &bearer,
            Method::GET,
            &format!("{ROOT}/entries/{id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{fresh}");
        assert_eq!(fresh["word"]["forms"], saved["word"]["forms"]);
        assert_eq!(fresh["word"]["forms"]["pos"], forms["pos"]);
        let mut actual_contents = fresh["word"]["forms"]["contents"]
            .as_array()
            .unwrap()
            .clone();
        let mut expected_contents = forms["contents"].as_array().unwrap().clone();
        actual_contents.sort_by_key(|content| content["id"].as_str().unwrap().to_owned());
        expected_contents.sort_by_key(|content| content["id"].as_str().unwrap().to_owned());
        assert_eq!(actual_contents, expected_contents);
        let form = &fresh["word"]["forms"]["pos"][0]["forms"][0];
        assert_eq!(form["id"], original_form["id"]);
        assert_eq!(
            fresh["word"]["forms"]["pos"][0]["form_groups"],
            original_forms["pos"][0]["form_groups"]
        );
        for side in ["uk", "us"] {
            assert_eq!(
                form["regional_bindings"][side]["id"],
                original_form["regional_bindings"][side]["id"]
            );
        }
        let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM lexicon.v3_form_contents WHERE entry_id=$1),(SELECT count(*) FROM lexicon.v3_concrete_forms WHERE entry_id=$1)")
            .bind(entry_id).fetch_one(&pool).await.unwrap();
        assert_eq!(
            counts,
            (2, 3),
            "旧内容仍有 verb 使用，保留两个内容且不增加词形"
        );
        let old_pronunciation = Uuid::parse_str(
            original_form["regional_bindings"]["uk"]["pronunciations"][0]["id"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let retired: bool = sqlx::query_scalar(
            "SELECT removed_from_draft_at IS NOT NULL FROM lexicon.nodes WHERE id=$1",
        )
        .bind(old_pronunciation)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(retired, target != 0);
    }
    let mut forms = original_forms.clone();
    reassign_form_content(&mut forms, 0, 0, 1);
    reassign_form_content(&mut forms, 1, 0, 1);
    let removed_content = forms["contents"].as_array_mut().unwrap().remove(0);
    save_shared(&state, &bearer, id, revision, forms.clone()).await;
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["forms"], forms);
    assert!(
        fresh["retired_content_slots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|slot| slot["id"] == removed_content["id"])
    );
}

#[sqlx::test]
async fn shared_forms_reassignment_rejects_stolen_nodes_and_new_content(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut forms = read["word"]["forms"].clone();
    forms["pos"][0]["forms"][1]["form_type"] = json!("plural");
    add_verb_binding(&mut forms);
    let saved = save_shared(
        &state,
        &bearer,
        id,
        legacy["word"]["revision"].as_i64().unwrap(),
        forms,
    )
    .await;
    let forms = &saved["word"]["forms"];
    let revision = saved["word"]["revision"].as_i64().unwrap();
    let mut moved = forms.clone();
    reassign_form_content(&mut moved, 0, 0, 1);
    let mut invalids = Vec::new();
    let mut old_pronunciation = moved.clone();
    old_pronunciation["pos"][0]["forms"][0]["regional_bindings"]["uk"]["pronunciations"][0]["id"] =
        forms["pos"][0]["forms"][0]["regional_bindings"]["uk"]["pronunciations"][0]["id"].clone();
    invalids.push(("旧发音换槽", old_pronunciation, "stable_node_id_changed"));
    let mut stolen_variant = moved.clone();
    stolen_variant["pos"][0]["forms"][0]["regional_bindings"]["uk"]["id"] =
        forms["pos"][1]["forms"][0]["regional_bindings"]["uk"]["id"].clone();
    // Remove its original occurrence so the identity guard, rather than duplicate-ID parsing, rejects it.
    stolen_variant["pos"].as_array_mut().unwrap().remove(1);
    stolen_variant["contents"].as_array_mut().unwrap().remove(0);
    invalids.push(("挪用其他词形变体", stolen_variant, "stable_node_id_changed"));
    let mut stolen_pronunciation = moved.clone();
    stolen_pronunciation["pos"][0]["forms"][0]["regional_bindings"]["uk"]["pronunciations"][0]["id"] =
        forms["pos"][1]["forms"][0]["regional_bindings"]["uk"]["pronunciations"][0]["id"].clone();
    stolen_pronunciation["pos"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    stolen_pronunciation["contents"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    invalids.push((
        "挪用其他词形发音",
        stolen_pronunciation,
        "validation_failed",
    ));
    let mut changed_role = moved.clone();
    let uk_id = changed_role["pos"][0]["forms"][0]["regional_bindings"]["uk"]["id"].clone();
    changed_role["pos"][0]["forms"][0]["regional_bindings"]["uk"]["id"] =
        changed_role["pos"][0]["forms"][0]["regional_bindings"]["us"]["id"].clone();
    changed_role["pos"][0]["forms"][0]["regional_bindings"]["us"]["id"] = uk_id;
    invalids.push(("英美角色互换", changed_role, "stable_node_id_changed"));
    let foreign = create_v3_with_annotated_complete_forms(&state, &pool, &bearer).await;
    let mut foreign_variant = moved.clone();
    foreign_variant["pos"][0]["forms"][0]["regional_bindings"]["uk"]["id"] =
        foreign["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["id"].clone();
    invalids.push(("跨词条变体", foreign_variant, "stable_node_id_changed"));
    let mut foreign_pronunciation = moved.clone();
    foreign_pronunciation["pos"][0]["forms"][0]["regional_bindings"]["uk"]["pronunciations"][0]["id"] =
        foreign["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["pronunciations"]
            [0]["id"]
            .clone();
    invalids.push(("跨词条发音", foreign_pronunciation, "validation_failed"));
    let mut same_content_slot = forms.clone();
    let new_slot = json!(Uuid::now_v7());
    same_content_slot["contents"][0]["regional_variants"]["uk"]["id"] = new_slot.clone();
    for pos in [0, 1] {
        same_content_slot["pos"][pos]["forms"][0]["regional_bindings"]["uk"]["content_slot_id"] =
            new_slot.clone();
    }
    invalids.push((
        "同内容变体换槽",
        same_content_slot,
        "stable_node_id_changed",
    ));
    let mut new_content = forms.clone();
    let mut target = forms["contents"][1].clone();
    target["id"] = json!(Uuid::now_v7());
    for side in ["uk", "us"] {
        target["regional_variants"][side]["id"] = json!(Uuid::now_v7());
        for row in target["regional_variants"][side]["pronunciations"]
            .as_array_mut()
            .unwrap()
        {
            row["id"] = json!(Uuid::now_v7());
        }
    }
    new_content["contents"].as_array_mut().unwrap().push(target);
    reassign_form_content(&mut new_content, 0, 0, 2);
    invalids.push(("切换至本次新造内容", new_content, "stable_node_id_changed"));
    for (case, invalid, code) in invalids {
        for (method, suffix) in [(Method::POST, "/impact"), (Method::PUT, "")] {
            let mut body = json!({"schema_version":3,"base_revision":revision,"content":invalid});
            if method == Method::PUT {
                body["intent"] = json!("save");
            }
            let (status, error) = shared_call(
                &state,
                &bearer,
                method,
                &format!("{ROOT}/entries/{id}/steps/forms{suffix}"),
                Some(body),
            )
            .await;
            let expected = if code == "validation_failed" {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::CONFLICT
            };
            assert_eq!(status, expected, "{case}: {error}");
            assert_eq!(error["code"], code, "{case}: {error}");
        }
    }
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["forms"], *forms);
    assert_eq!(fresh["word"]["revision"], revision);
}

#[sqlx::test]
async fn shared_forms_deleted_form_cannot_use_new_sides_to_bind_new_content(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut forms = read["word"]["forms"].clone();
    let form_id = json!(Uuid::now_v7());
    let membership = json!({"id":Uuid::now_v7(),"form_id":form_id});
    let mut common = forms["contents"][0]["regional_variants"]["uk"].clone();
    common["id"] = json!(Uuid::now_v7());
    for row in common["pronunciations"].as_array_mut().unwrap() {
        row["id"] = json!(Uuid::now_v7());
    }
    let common_content = json!({"id":Uuid::now_v7(),"dialect_rules":{"spelling_mode":"unified","phonetic_mode":"unified"},"regional_variants":{"mode":"common","common":common}});
    let common_refs: Vec<_> = common["pronunciations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| json!({"id":Uuid::now_v7(),"content_slot_id":row["id"]}))
        .collect();
    let binding = json!({"id":form_id,"form_type":"plural","content_id":common_content["id"],"regional_bindings":{"mode":"common","common":{"id":Uuid::now_v7(),"content_slot_id":common["id"],"is_regular":true,"pronunciations":common_refs}}});
    forms["contents"]
        .as_array_mut()
        .unwrap()
        .push(common_content.clone());
    forms["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .push(binding.clone());
    forms["pos"][0]["form_groups"][0]["members"]
        .as_array_mut()
        .unwrap()
        .push(membership.clone());
    let created = save_shared(
        &state,
        &bearer,
        id,
        legacy["word"]["revision"].as_i64().unwrap(),
        forms,
    )
    .await;
    let mut deleted = created["word"]["forms"].clone();
    deleted["contents"]
        .as_array_mut()
        .unwrap()
        .retain(|row| row["id"] != common_content["id"]);
    deleted["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .retain(|row| row["id"] != form_id);
    deleted["pos"][0]["form_groups"][0]["members"]
        .as_array_mut()
        .unwrap()
        .retain(|row| row["form_id"] != form_id);
    let deleted = save_shared(
        &state,
        &bearer,
        id,
        created["word"]["revision"].as_i64().unwrap(),
        deleted,
    )
    .await;
    let revision = deleted["word"]["revision"].as_i64().unwrap();
    let mut proposed = deleted["word"]["forms"].clone();
    let mut target = proposed["contents"][0].clone();
    target["id"] = json!(Uuid::now_v7());
    let mut regional = json!({"mode":"uk_us"});
    for side in ["uk", "us"] {
        target["regional_variants"][side]["id"] = json!(Uuid::now_v7());
        let rows = target["regional_variants"][side]["pronunciations"]
            .as_array_mut()
            .unwrap();
        for row in rows.iter_mut() {
            row["id"] = json!(Uuid::now_v7());
        }
        regional[side] = json!({"id":Uuid::now_v7(),"content_slot_id":target["regional_variants"][side]["id"],"is_regular":true,"pronunciations":[]});
    }
    proposed["contents"].as_array_mut().unwrap().push(target);
    let mut restored = binding;
    restored["regional_bindings"] = regional;
    proposed["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .push(restored);
    proposed["pos"][0]["form_groups"][0]["members"]
        .as_array_mut()
        .unwrap()
        .push(membership);
    reassign_form_content(&mut proposed, 0, 2, 2);
    for (method, suffix) in [(Method::POST, "/impact"), (Method::PUT, "")] {
        let mut body = json!({"schema_version":3,"base_revision":revision,"content":proposed});
        if method == Method::PUT {
            body["intent"] = json!("save");
        }
        let (status, rejected) = shared_call(
            &state,
            &bearer,
            method,
            &format!("{ROOT}/entries/{id}/steps/forms{suffix}"),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert_eq!(rejected["code"], "stable_node_id_changed");
    }
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["revision"], revision);
    assert_eq!(fresh["word"]["forms"], deleted["word"]["forms"]);
    // A genuinely new form may use new content in the same request.
    proposed["pos"][0]["forms"][2]["id"] = json!(Uuid::now_v7());
    proposed["pos"][0]["form_groups"][0]["members"][2] =
        json!({"id":Uuid::now_v7(),"form_id":proposed["pos"][0]["forms"][2]["id"]});
    let accepted = save_shared(&state, &bearer, id, revision, proposed).await;
    assert_eq!(
        accepted["word"]["forms"]["pos"][0]["forms"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[sqlx::test]
async fn shared_forms_reassignment_restores_retired_sides_across_modes(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut forms = read["word"]["forms"].clone();
    forms["pos"][0]["forms"][1]["form_type"] = json!("plural");
    add_verb_binding(&mut forms);
    let mut common = forms["contents"][0]["regional_variants"]["uk"].clone();
    common["id"] = json!(Uuid::now_v7());
    common["pronunciations"][0]["id"] = json!(Uuid::now_v7());
    let common_content = json!({"id":Uuid::now_v7(),"dialect_rules":{"spelling_mode":"unified","phonetic_mode":"unified"},"regional_variants":{"mode":"common","common":common}});
    forms["contents"]
        .as_array_mut()
        .unwrap()
        .push(common_content.clone());
    let mut verb_common = forms["pos"][1]["forms"][0]["regional_bindings"]["uk"].clone();
    verb_common["id"] = json!(Uuid::now_v7());
    forms["pos"][1]["forms"][0]["regional_bindings"] =
        json!({"mode":"common","common":verb_common});
    reassign_form_content(&mut forms, 1, 0, 2);
    let saved = save_shared(
        &state,
        &bearer,
        id,
        legacy["word"]["revision"].as_i64().unwrap(),
        forms,
    )
    .await;
    let original_regions =
        saved["word"]["forms"]["pos"][0]["forms"][0]["regional_bindings"].clone();
    let target_split = saved["word"]["forms"]["contents"][1]["id"].clone();
    let mut current = saved;
    let mut own_common = original_regions["uk"].clone();
    own_common["id"] = json!(Uuid::now_v7());
    for to_common in [true, false, true] {
        let mut forms = current["word"]["forms"].clone();
        let target_id = if to_common {
            &common_content["id"]
        } else {
            &target_split
        };
        let target = forms["contents"]
            .as_array()
            .unwrap()
            .iter()
            .position(|content| &content["id"] == target_id)
            .unwrap();
        forms["pos"][0]["forms"][0]["regional_bindings"] = if to_common {
            json!({"mode":"common","common":own_common})
        } else {
            original_regions.clone()
        };
        reassign_form_content(&mut forms, 0, 0, target);
        let used: Vec<_> = forms["pos"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|pos| pos["forms"].as_array().unwrap())
            .map(|form| form["content_id"].clone())
            .collect();
        forms["contents"]
            .as_array_mut()
            .unwrap()
            .retain(|content| used.contains(&content["id"]));
        current = save_shared(
            &state,
            &bearer,
            id,
            current["word"]["revision"].as_i64().unwrap(),
            forms,
        )
        .await;
        let (_, fresh) = shared_call(
            &state,
            &bearer,
            Method::GET,
            &format!("{ROOT}/entries/{id}"),
            None,
        )
        .await;
        assert_eq!(fresh["word"]["forms"], current["word"]["forms"]);
        let regions = &fresh["word"]["forms"]["pos"][0]["forms"][0]["regional_bindings"];
        if to_common {
            assert_eq!(regions["common"]["id"], own_common["id"]);
            assert!(
                fresh["retired_stable_nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|node| node["id"] == original_regions["uk"]["id"])
            );
        } else {
            for side in ["uk", "us"] {
                assert_eq!(regions[side]["id"], original_regions[side]["id"]);
            }
            assert!(
                fresh["retired_stable_nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|node| node["id"] == own_common["id"])
            );
        }
    }
}

#[sqlx::test]
async fn shared_forms_reassignment_review_mode_change_restores_old_sides(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut forms = read["word"]["forms"].clone();
    forms["pos"][0]["forms"][1]["form_type"] = json!("plural");
    let original_regions = forms["pos"][0]["forms"][0]["regional_bindings"].clone();
    let mut common = forms["contents"][1]["regional_variants"]["uk"].clone();
    common["id"] = json!(Uuid::now_v7());
    common["pronunciations"][0]["id"] = json!(Uuid::now_v7());
    forms["contents"][1]["regional_variants"] = json!({"mode":"common","common":common});
    forms["contents"][1]["dialect_rules"] =
        json!({"spelling_mode":"unified","phonetic_mode":"unified"});
    let mut common_ref = forms["pos"][0]["forms"][1]["regional_bindings"]["uk"].clone();
    common_ref["id"] = json!(Uuid::now_v7());
    forms["pos"][0]["forms"][1]["regional_bindings"] = json!({"mode":"common","common":common_ref});
    reassign_form_content(&mut forms, 0, 1, 1);
    let saved = save_shared(
        &state,
        &bearer,
        id,
        legacy["word"]["revision"].as_i64().unwrap(),
        forms,
    )
    .await;
    let target_regions = saved["word"]["forms"]["pos"][0]["forms"][1]["regional_bindings"].clone();
    let mut moved = saved["word"]["forms"].clone();
    common_ref["id"] = json!(Uuid::now_v7());
    moved["pos"][0]["forms"][0]["regional_bindings"] = json!({"mode":"common","common":common_ref});
    reassign_form_content(&mut moved, 0, 0, 1);
    moved["contents"].as_array_mut().unwrap().remove(0);
    let moved = save_shared(
        &state,
        &bearer,
        id,
        saved["word"]["revision"].as_i64().unwrap(),
        moved,
    )
    .await;
    let mut split = moved["word"]["forms"].clone();
    // Restore B's permanent regional slots, not newly allocated identities.
    split["contents"][0]["regional_variants"] =
        read["word"]["forms"]["contents"][1]["regional_variants"].clone();
    split["contents"][0]["dialect_rules"] =
        read["word"]["forms"]["contents"][1]["dialect_rules"].clone();
    split["pos"][0]["forms"][0]["regional_bindings"] = original_regions.clone();
    // The second form's original UK/US identities are in the server's retired-node ledger.
    let (_, original_read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let target_form = split["pos"][0]["forms"][1]["id"].clone();
    let mut second = json!({"mode":"uk_us"});
    for side in ["uk", "us"] {
        let role = format!("{side}_variant");
        let node = original_read["retired_stable_nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["parent_node_id"] == target_form && node["node_role"] == role)
            .unwrap();
        let mut value = target_regions["common"].clone();
        value["id"] = node["id"].clone();
        second[side] = value;
    }
    split["pos"][0]["forms"][1]["regional_bindings"] = second;
    reassign_form_content(&mut split, 0, 0, 0);
    reassign_form_content(&mut split, 0, 1, 0);
    let split = save_shared(
        &state,
        &bearer,
        id,
        moved["word"]["revision"].as_i64().unwrap(),
        split,
    )
    .await;
    for side in ["uk", "us"] {
        assert_eq!(
            split["word"]["forms"]["pos"][0]["forms"][0]["regional_bindings"][side]["id"],
            original_regions[side]["id"]
        );
    }
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["forms"], split["word"]["forms"]);
}

#[sqlx::test]
async fn shared_forms_reassignment_keeps_self_reference_impact_and_save_guards(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let source = self_linked_definition_draft(&state, &pool, &bearer).await;
    let id = source["word"]["id"].as_str().unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut forms = read["word"]["forms"].clone();
    forms["pos"][0]["forms"][1]["form_type"] = json!("plural");
    let mut common = forms["contents"][1]["regional_variants"]["uk"].clone();
    common["id"] = json!(Uuid::now_v7());
    common["pronunciations"][0]["id"] = json!(Uuid::now_v7());
    forms["contents"][1]["regional_variants"] = json!({"mode":"common","common":common});
    forms["contents"][1]["dialect_rules"] =
        json!({"spelling_mode":"unified","phonetic_mode":"unified"});
    let mut second_common = forms["pos"][0]["forms"][1]["regional_bindings"]["uk"].clone();
    second_common["id"] = json!(Uuid::now_v7());
    forms["pos"][0]["forms"][1]["regional_bindings"] =
        json!({"mode":"common","common":second_common});
    reassign_form_content(&mut forms, 0, 1, 1);
    let saved = save_shared(
        &state,
        &bearer,
        id,
        source["word"]["revision"].as_i64().unwrap(),
        forms,
    )
    .await;
    let mut moved = saved["word"]["forms"].clone();
    let mut own_common = moved["pos"][0]["forms"][1]["regional_bindings"]["common"].clone();
    own_common["id"] = json!(Uuid::now_v7());
    moved["pos"][0]["forms"][0]["regional_bindings"] = json!({"mode":"common","common":own_common});
    reassign_form_content(&mut moved, 0, 0, 1);
    moved["contents"].as_array_mut().unwrap().remove(0);
    let revision = saved["word"]["revision"].as_i64().unwrap();
    let (status, impact) = shared_call(
        &state,
        &bearer,
        Method::POST,
        &format!("{ROOT}/entries/{id}/steps/forms/impact"),
        Some(json!({"schema_version":3,"base_revision":revision,"content":moved})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert!(
        impact["blocked_references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reference| reference["source"]["entry_id"] == id),
        "{impact}"
    );
    let mut body =
        json!({"schema_version":3,"base_revision":revision,"intent":"save","content":moved});
    if let Some(token) = impact["confirmation_token"].as_str() {
        body["confirmed_impact_token"] = json!(token);
    }
    for (source, target) in [
        (
            "surface_confirmation_token",
            "confirmed_surface_match_token",
        ),
        ("impact_confirmation_token", "confirmed_impact_token"),
    ] {
        if let Some(token) = impact["surface_match_page"][source].as_str() {
            body[target] = json!(token);
        }
    }
    let (status, rejected) = shared_call(
        &state,
        &bearer,
        Method::PUT,
        &format!("{ROOT}/entries/{id}/steps/forms"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(rejected["code"], "inbound_reference_conflict", "{rejected}");
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["forms"], saved["word"]["forms"]);
    assert_eq!(fresh["word"]["revision"], revision);
}

#[sqlx::test]
async fn shared_forms_concurrent_saves_accept_one_graph_without_loser_slots(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut initial = read["word"]["forms"].clone();
    add_verb_binding(&mut initial);
    let saved = save_shared(
        &state,
        &bearer,
        id,
        legacy["word"]["revision"].as_i64().unwrap(),
        initial.clone(),
    )
    .await;
    let revision = saved["word"]["revision"].as_i64().unwrap();
    let candidate = |reading: &str| {
        let mut forms = initial.clone();
        let mut row = forms["contents"][0]["regional_variants"]["uk"]["pronunciations"][0].clone();
        let slot = Uuid::now_v7();
        row["id"] = json!(slot);
        row["actual_pron"] = json!(reading);
        forms["contents"][0]["regional_variants"]["uk"]["pronunciations"]
            .as_array_mut()
            .unwrap()
            .push(row);
        for pos in forms["pos"].as_array_mut().unwrap() {
            for form in pos["forms"].as_array_mut().unwrap() {
                if form["content_id"] == initial["contents"][0]["id"] {
                    form["regional_bindings"]["uk"]["pronunciations"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"id":Uuid::now_v7(),"content_slot_id":slot}));
                }
            }
        }
        (slot, forms)
    };
    let (left_slot, left) = candidate("left writer");
    let (right_slot, right) = candidate("right writer");
    let path = format!("{ROOT}/entries/{id}/steps/forms");
    let (left_result, right_result) = tokio::join!(
        shared_call(
            &state,
            &bearer,
            Method::PUT,
            &path,
            Some(
                json!({"schema_version":3,"base_revision":revision,"intent":"save","content":left})
            )
        ),
        shared_call(
            &state,
            &bearer,
            Method::PUT,
            &path,
            Some(
                json!({"schema_version":3,"base_revision":revision,"intent":"save","content":right})
            )
        )
    );
    let (winner, loser_slot, failure) = if left_result.0 == StatusCode::OK {
        assert_eq!(right_result.0, StatusCode::CONFLICT, "{:?}", right_result);
        (left, right_slot, right_result.1)
    } else {
        assert_eq!(left_result.0, StatusCode::CONFLICT, "{:?}", left_result);
        assert_eq!(right_result.0, StatusCode::OK, "{:?}", right_result);
        (right, left_slot, left_result.1)
    };
    assert_eq!(failure["code"], "revision_conflict");
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["revision"], revision + 1);
    assert_eq!(fresh["word"]["forms"], winner);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.v3_form_content_slots WHERE id=$1")
            .bind(loser_slot)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0, "失败请求不得留下内容槽位");
}

#[sqlx::test]
async fn shared_forms_slot_retirement_restore_and_reassignment_are_persistent(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut content = read["word"]["forms"].clone();
    add_verb_binding(&mut content);
    let original = save_shared(
        &state,
        &bearer,
        id,
        legacy["word"]["revision"].as_i64().unwrap(),
        content,
    )
    .await;
    let mut removed = original["word"]["forms"].clone();
    let slot_id =
        removed["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["id"].clone();
    let reference_id =
        removed["pos"][0]["forms"][0]["regional_bindings"]["uk"]["pronunciations"][0]["id"].clone();
    removed["contents"][0]["regional_variants"]["uk"]["pronunciations"] = json!([]);
    removed["pos"][0]["forms"][0]["regional_bindings"]["uk"]["pronunciations"] = json!([]);
    removed["pos"][1]["forms"][0]["regional_bindings"]["uk"]["pronunciations"] = json!([]);
    let removed = save_shared(
        &state,
        &bearer,
        id,
        original["word"]["revision"].as_i64().unwrap(),
        removed,
    )
    .await;
    let (_, ledger) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert!(
        ledger["retired_content_slots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|slot| slot["id"] == slot_id)
    );
    assert!(
        ledger["retired_stable_nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["id"] == reference_id
                && node["content_slot_id"] == slot_id
                && node["content_id"] == original["word"]["forms"]["contents"][0]["id"])
    );
    let restored = save_shared(
        &state,
        &bearer,
        id,
        removed["word"]["revision"].as_i64().unwrap(),
        original["word"]["forms"].clone(),
    )
    .await;
    let mut moved = restored["word"]["forms"].clone();
    let new_slot = json!(Uuid::now_v7());
    moved["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["id"] = new_slot.clone();
    for pos_index in [0, 1] {
        moved["pos"][pos_index]["forms"][0]["regional_bindings"]["uk"]["pronunciations"][0]["content_slot_id"] =
            new_slot.clone();
    }
    let (status, error) = shared_call(&state, &bearer, Method::PUT, &format!("{ROOT}/entries/{id}/steps/forms"), Some(json!({"schema_version":3,"base_revision":restored["word"]["revision"],"intent":"save","content":moved}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["code"], "stable_node_id_changed");
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["forms"], original["word"]["forms"]);
    assert_eq!(fresh["word"]["revision"], restored["word"]["revision"]);
}

#[sqlx::test]
async fn shared_forms_pronunciation_correspondence_persists_and_cannot_be_reassigned(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let entry_id = Uuid::parse_str(id).unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut forms = read["word"]["forms"].clone();
    add_verb_binding(&mut forms);
    let common = json!({"id":Uuid::now_v7(),"variant_slot_id":Uuid::now_v7()});
    let mapping = json!({
        "id":Uuid::now_v7(),"common":common,
        "uk":{"id":forms["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["id"],"variant_slot_id":forms["contents"][0]["regional_variants"]["uk"]["id"]},
        "us":{"id":forms["contents"][0]["regional_variants"]["us"]["pronunciations"][0]["id"],"variant_slot_id":forms["contents"][0]["regional_variants"]["us"]["id"]}
    });
    forms["contents"][0]["pronunciation_slots"] = json!([mapping]);
    let saved = save_shared(
        &state,
        &bearer,
        id,
        legacy["word"]["revision"].as_i64().unwrap(),
        forms.clone(),
    )
    .await;
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["forms"], forms);
    let retired: (Uuid, String, bool) = sqlx::query_as("SELECT parent_slot_id,role,retired_at IS NOT NULL FROM lexicon.v3_form_content_slots WHERE id=$1")
        .bind(Uuid::parse_str(common["id"].as_str().unwrap()).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(
        retired,
        (
            Uuid::parse_str(common["variant_slot_id"].as_str().unwrap()).unwrap(),
            "pronunciation".into(),
            true
        )
    );
    let revision = saved["word"]["revision"].as_i64().unwrap();
    for change_mapping in [false, true] {
        let mut invalid = forms.clone();
        if change_mapping {
            invalid["contents"][0]["pronunciation_slots"][0]["id"] = json!(Uuid::now_v7());
        } else {
            invalid["contents"][0]["pronunciation_slots"][0]["common"]["id"] =
                json!(Uuid::now_v7());
        }
        for (method, suffix) in [(Method::POST, "/impact"), (Method::PUT, "")] {
            let mut body = json!({"schema_version":3,"base_revision":revision,"content":invalid});
            if method == Method::PUT {
                body["intent"] = json!("save");
            }
            let (status, error) = shared_call(
                &state,
                &bearer,
                method,
                &format!("{ROOT}/entries/{id}/steps/forms{suffix}"),
                Some(body),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{error}");
            assert_eq!(error["code"], "stable_node_id_changed");
        }
    }
    let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM lexicon.v3_form_pronunciation_mappings WHERE entry_id=$1),(SELECT count(*) FROM lexicon.v3_form_pronunciation_mapping_slots WHERE entry_id=$1)")
        .bind(entry_id).fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (1, 3), "失败事务不得留下映射");
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["forms"], forms);
    assert_eq!(fresh["word"]["revision"], revision);
    let mut revision = revision;
    for omit_all in [true, false] {
        let mut omitted = forms.clone();
        if omit_all {
            omitted["contents"][0]
                .as_object_mut()
                .unwrap()
                .remove("pronunciation_slots");
        } else {
            omitted["contents"][0]["pronunciation_slots"][0]
                .as_object_mut()
                .unwrap()
                .remove("common");
        }
        let saved = save_shared(&state, &bearer, id, revision, omitted).await;
        assert_eq!(
            saved["word"]["forms"], forms,
            "省略身份元数据应从永久账本补回"
        );
        revision = saved["word"]["revision"].as_i64().unwrap();
        let (_, fresh) = shared_call(
            &state,
            &bearer,
            Method::GET,
            &format!("{ROOT}/entries/{id}"),
            None,
        )
        .await;
        assert_eq!(fresh["word"]["forms"], forms);
    }
    let error = sqlx::raw_sql(include_str!(
        "../../migrations/20261020220000_form_pronunciation_slots.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("lossy rollback"));
}

#[sqlx::test]
async fn shared_forms_reassignment_review_restore_retired_content(pool: PgPool) {
    reassignment_restore_history(pool, false, false).await;
}

#[sqlx::test]
async fn shared_forms_reassignment_review_restore_deleted_form(pool: PgPool) {
    reassignment_restore_history(pool, true, false).await;
}

#[sqlx::test]
async fn shared_forms_restore_pre_shared_publication_with_unregistered_content(pool: PgPool) {
    reassignment_restore_history(pool, true, true).await;
}

async fn reassignment_restore_history(pool: PgPool, delete_form: bool, legacy_snapshot: bool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let entry_id = Uuid::parse_str(id).unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut forms = read["word"]["forms"].clone();
    // F is removable; another base keeps the group valid. H keeps A active in the deleted-form case.
    forms["pos"][0]["forms"][0]["form_type"] = json!("plural");
    if delete_form && !legacy_snapshot {
        let mut h = forms["pos"][0]["forms"][0].clone();
        h["id"] = json!(Uuid::now_v7());
        h["form_type"] = json!("base");
        for side in ["uk", "us"] {
            h["regional_bindings"][side]["id"] = json!(Uuid::now_v7());
            h["regional_bindings"][side]["pronunciations"][0]["id"] = json!(Uuid::now_v7());
        }
        forms["pos"][0]["form_groups"][0]["members"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":Uuid::now_v7(),"form_id":h["id"]}));
        forms["pos"][0]["forms"].as_array_mut().unwrap().push(h);
    }
    let saved = save_shared(
        &state,
        &bearer,
        id,
        legacy["word"]["revision"].as_i64().unwrap(),
        forms,
    )
    .await;
    let meanings = complete_v3_meanings_fixture(saved["word"]["forms"]["pos"][0]["pos_id"].clone());
    let ready = save_v3_meanings(&state, &bearer, &saved, meanings).await;
    let other = batch3_word(&state, &bearer, "beta").await;
    let (status, published) = call_with_form_capability(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/publications/batch"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(batch3_input(&[&ready, &other])),
        (true, true, true, true),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    let publication_id: Uuid = sqlx::query_scalar("SELECT id FROM lexicon.entry_publications WHERE entry_id=$1 ORDER BY publication_number DESC LIMIT 1").bind(entry_id).fetch_one(&pool).await.unwrap();
    let (_, original) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let original_forms = original["word"]["forms"].clone();
    let mut expected_forms = original_forms.clone();
    if legacy_snapshot {
        let expanded: tsz_rust::lexicon::dto::DraftFormsStepContentV3 =
            serde_json::from_value(original_forms.clone()).unwrap();
        let mut snapshot = original["word"].clone();
        snapshot["forms"] = json!({"pos": expanded.pos});
        expected_forms = sqlx::query_scalar("SELECT lexicon.v3_legacy_forms_to_shared($1)")
            .bind(&snapshot["forms"])
            .fetch_one(&pool)
            .await
            .unwrap();
        // Emulate an immutable expanded publication that predates the shared-content migration.
        sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=$1")
            .bind(publication_id)
            .bind(snapshot)
            .execute(&pool)
            .await
            .unwrap();
    }
    let mut moved = original_forms.clone();
    reassign_form_content(&mut moved, 0, 0, 1);
    if !delete_form || legacy_snapshot {
        moved["contents"].as_array_mut().unwrap().remove(0);
    }
    let mut moved = save_shared(
        &state,
        &bearer,
        id,
        original["word"]["revision"].as_i64().unwrap(),
        moved,
    )
    .await;
    if delete_form {
        let mut forms = moved["word"]["forms"].clone();
        let removed = forms["pos"][0]["forms"].as_array_mut().unwrap().remove(0);
        forms["pos"][0]["form_groups"][0]["members"]
            .as_array_mut()
            .unwrap()
            .retain(|member| member["form_id"] != removed["id"]);
        moved = save_shared(
            &state,
            &bearer,
            id,
            moved["word"]["revision"].as_i64().unwrap(),
            forms,
        )
        .await;
    }
    let (status, archived) = call_with_form_capability(&state, Method::POST, &format!("{ROOT}/entries/{id}/archive"), &bearer, Some(Uuid::now_v7()), Some(json!({"base_revision":moved["word"]["revision"],"base_lifecycle_revision":moved["word"]["lifecycle_revision"]})), (true,true,true,true), true).await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    let (status, draft) = call_with_form_capability(&state, Method::POST, &format!("{ROOT}/entries/{id}/restore"), &bearer, Some(Uuid::now_v7()), Some(json!({"restore_as":"draft","base_revision":archived["word"]["revision"],"base_lifecycle_revision":archived["word"]["lifecycle_revision"]})), (true,true,true,true), true).await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    if legacy_snapshot {
        let removed_content =
            Uuid::parse_str(original_forms["contents"][0]["id"].as_str().unwrap()).unwrap();
        // A migration before any shared binding existed would not have created this ledger.
        sqlx::query("DELETE FROM lexicon.v3_form_content_node_slots WHERE entry_id=$1 AND node_id IN (SELECT id FROM lexicon.nodes WHERE entry_id=$1 AND removed_from_draft_at IS NOT NULL)").bind(entry_id).execute(&pool).await.unwrap();
        sqlx::query(
            "DELETE FROM lexicon.v3_form_content_slots WHERE entry_id=$1 AND content_id=$2",
        )
        .bind(entry_id)
        .bind(removed_content)
        .execute(&pool)
        .await
        .unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM lexicon.v3_form_content_slots WHERE id=$1")
                .bind(removed_content)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
    }
    let restore_key = Uuid::now_v7();
    let input = json!({"schema_version":3,"base_revision":draft["word"]["revision"],"base_lifecycle_revision":draft["word"]["lifecycle_revision"]});
    for _ in 0..2 {
        let (status, restored) = call_with_form_capability(
            &state,
            Method::POST,
            &format!("{ROOT}/entries/{id}/publications/{publication_id}/restore-draft"),
            &bearer,
            Some(restore_key),
            Some(input.clone()),
            (true, true, true, true),
            true,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "delete_form={delete_form}: {restored}"
        );
        assert_eq!(restored["word"]["forms"], expected_forms);
        assert_eq!(
            restored["word"]["revision"].as_i64(),
            Some(draft["word"]["revision"].as_i64().unwrap() + 1)
        );
    }
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["forms"], expected_forms);
}

#[sqlx::test]
async fn shared_forms_publication_freezes_graph_and_restore_recovers_sharing(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let legacy = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let id = legacy["word"]["id"].as_str().unwrap();
    let (_, read) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    let mut content = read["word"]["forms"].clone();
    let variants = &content["contents"][0]["regional_variants"];
    content["contents"][0]["pronunciation_slots"] = json!([{
        "id": Uuid::now_v7(),
        "uk": {"id": variants["uk"]["pronunciations"][0]["id"], "variant_slot_id": variants["uk"]["id"]},
        "us": {"id": variants["us"]["pronunciations"][0]["id"], "variant_slot_id": variants["us"]["id"]}
    }]);
    let mut same_pos = content["pos"][0]["forms"][0].clone();
    same_pos["id"] = json!(Uuid::now_v7());
    same_pos["form_type"] = json!("plural");
    for side in ["uk", "us"] {
        same_pos["regional_bindings"][side]["id"] = json!(Uuid::now_v7());
        same_pos["regional_bindings"][side]["pronunciations"][0]["id"] = json!(Uuid::now_v7());
    }
    content["pos"][0]["form_groups"][0]["members"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":Uuid::now_v7(),"form_id":same_pos["id"]}));
    content["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .push(same_pos);
    let shared = save_shared(
        &state,
        &bearer,
        id,
        legacy["word"]["revision"].as_i64().unwrap(),
        content,
    )
    .await;
    let meanings =
        complete_v3_meanings_fixture(shared["word"]["forms"]["pos"][0]["pos_id"].clone());
    let ready = save_v3_meanings(&state, &bearer, &shared, meanings).await;
    let other = batch3_word(&state, &bearer, "beta").await;
    let batch_key = Uuid::now_v7();
    let batch_input = batch3_input(&[&ready, &other]);
    let batch_path = format!("{ROOT}/entries/publications/batch");
    let (status, batch) = call_with_form_capability(
        &state,
        Method::POST,
        &batch_path,
        &bearer,
        Some(batch_key),
        Some(batch_input.clone()),
        (true, true, true, true),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{batch}");
    let (status, replayed) = call_with_form_capability(
        &state,
        Method::POST,
        &batch_path,
        &bearer,
        Some(batch_key),
        Some(batch_input),
        (true, true, true, true),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replayed}");
    assert_eq!(replayed, batch);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publications")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
    let published = json!({"word":batch["words"][0]});
    let (publication_id, snapshot, hash): (Uuid, Value, Vec<u8>) = sqlx::query_as("SELECT id,snapshot,snapshot_hash FROM lexicon.entry_publications WHERE entry_id=$1 ORDER BY publication_number DESC LIMIT 1").bind(Uuid::parse_str(id).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(snapshot["forms"]["forms_format"], "shared_v1");
    assert_eq!(snapshot["forms"]["contents"].as_array().unwrap().len(), 2);
    assert_eq!(
        snapshot["forms"]["pos"][0]["forms"][0]["content_id"],
        snapshot["forms"]["pos"][0]["forms"][2]["content_id"]
    );
    let mut edited = snapshot["forms"].clone();
    edited["contents"][0]["regional_variants"]["uk"]["pronunciations"][0]["actual_pron"] =
        json!("draft only");
    let edited = save_shared(
        &state,
        &bearer,
        id,
        published["word"]["revision"].as_i64().unwrap(),
        edited,
    )
    .await;
    let edited_forms = edited["word"]["forms"].clone();
    let (stored, stored_hash): (Value, Vec<u8>) =
        sqlx::query_as("SELECT snapshot,snapshot_hash FROM lexicon.entry_publications WHERE id=$1")
            .bind(publication_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((stored, stored_hash), (snapshot.clone(), hash));
    let rollback_path = format!("{ROOT}/entries/{id}/publications/{publication_id}/rollback");
    let rollback_key = Uuid::now_v7();
    let rollback_input = json!({"schema_version":3,"base_revision":edited["word"]["revision"],"base_lifecycle_revision":edited["word"]["lifecycle_revision"]});
    let (status, rolled_back) = call_with_form_capability(
        &state,
        Method::POST,
        &rollback_path,
        &bearer,
        Some(rollback_key),
        Some(rollback_input.clone()),
        (true, true, true, true),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{rolled_back}");
    let (status, replayed) = call_with_form_capability(
        &state,
        Method::POST,
        &rollback_path,
        &bearer,
        Some(rollback_key),
        Some(rollback_input),
        (true, true, true, true),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replayed}");
    assert_eq!(replayed, rolled_back);
    let entry_id = Uuid::parse_str(id).unwrap();
    assert_eq!(
        current_publication_snapshot(&pool, entry_id).await["forms"],
        snapshot["forms"]
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publications WHERE entry_id=$1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 2, "回滚重放不得新增发布记录");
    let (_, edited) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(edited["word"]["forms"], edited_forms);
    let (status, archived) = call_with_form_capability(&state, Method::POST, &format!("{ROOT}/entries/{id}/archive"), &bearer, Some(Uuid::now_v7()), Some(json!({"base_revision":edited["word"]["revision"],"base_lifecycle_revision":edited["word"]["lifecycle_revision"]})), (true,true,true,true), true).await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    let (status, draft) = call_with_form_capability(&state, Method::POST, &format!("{ROOT}/entries/{id}/restore"), &bearer, Some(Uuid::now_v7()), Some(json!({"restore_as":"draft","base_revision":archived["word"]["revision"],"base_lifecycle_revision":archived["word"]["lifecycle_revision"]})), (true,true,true,true), true).await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    let (status, restored) = call_with_form_capability(&state, Method::POST, &format!("{ROOT}/entries/{id}/publications/{publication_id}/restore-draft"), &bearer, Some(Uuid::now_v7()), Some(json!({"schema_version":3,"base_revision":draft["word"]["revision"],"base_lifecycle_revision":draft["word"]["lifecycle_revision"]})), (true,true,true,true), true).await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["word"]["forms"], snapshot["forms"]);
    let (_, fresh) = shared_call(
        &state,
        &bearer,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        None,
    )
    .await;
    assert_eq!(fresh["word"]["forms"], snapshot["forms"]);
}
