use super::*;

async fn request(
    state: &AppState,
    actor: Option<Uuid>,
    method: &str,
    path: &str,
    body: Value,
    negotiated: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("{ROOT}{path}"))
        .header("content-type", "application/json")
        .header("x-tsz-next-medium-ownership", "v1");
    if negotiated {
        req = req.header("x-tsz-next-relations", "v1");
    }
    if let Some(actor) = actor {
        req = req.header(
            "authorization",
            format!(
                "Bearer {}",
                state.admin_token_manager.generate(actor, "admin").unwrap()
            ),
        );
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn entry(state: &AppState, actor: Uuid, medium: &str) -> Value {
    let (status, created) = request(
        state,
        Some(actor),
        "POST",
        "/entries",
        json!({"target_language":"en", "medium_locale":medium, "kind":"word", "headword":"work"}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let mut draft = created["draft"].clone();
    draft["senses"] =
        json!([{"id":Uuid::now_v7(),"part_of_speech":"noun","definitions":[],"equivalents":[]}]);
    let (status, saved) = request(
        state,
        Some(actor),
        "PUT",
        &format!("/entries/{}", created["id"].as_str().unwrap()),
        json!({"expected_revision":1,"draft":draft}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved
}

#[sqlx::test]
async fn relations_roundtrip_legacy_preservation_and_reference_protection(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../../migrations/20261115160000_lexicon_next_relations.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261115160000_lexicon_next_relations.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let actor = admin(&pool, EDIT).await;
    let reader = admin(&pool, &["words.access"]).await;
    let state = AppState::for_test(pool.clone());
    let target = entry(&state, actor, "zh-Hans").await;
    let source = entry(&state, actor, "zh-Hans").await;
    let path = format!("/entries/{}", source["id"].as_str().unwrap());
    let mut draft = source["draft"].clone();
    let rows = json!([
        {"id":Uuid::now_v7(),"relation":"synonym","target_word_id":target["id"],"target_sense_id":target["draft"]["senses"][0]["id"],"score":"87.5"},
        {"id":Uuid::now_v7(),"relation":"antonym","pending_target_headword":"leisure","pending_target_gloss":"闲暇","score":"71"},
        {"id":Uuid::now_v7(),"relation":"derivative","pending_target_headword":"worker","pending_target_gloss":"工人","score":"90"}
    ]);
    draft["senses"][0]["relations"] = rows.clone();
    let body = json!({"expected_revision":2,"draft":draft});
    assert_eq!(
        request(&state, None, "PUT", &path, body.clone(), true)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&state, Some(reader), "PUT", &path, body.clone(), true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&state, Some(actor), "PUT", &path, body.clone(), false)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (status, saved) = request(&state, Some(actor), "PUT", &path, body, true).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, reloaded) = request(&state, Some(actor), "GET", &path, Value::Null, true).await;
    assert_eq!(reloaded["draft"]["senses"][0]["relations"], rows);
    assert!(
        sqlx::raw_sql(include_str!(
            "../../migrations/20261115160000_lexicon_next_relations.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
    let (_, view) = request(
        &state,
        Some(actor),
        "GET",
        &format!("{path}/view?target_locale=en-GB&medium_locale=zh-Hans"),
        Value::Null,
        true,
    )
    .await;
    assert_eq!(view["senses"][0]["relations"], rows);
    let (_, legacy) = request(&state, Some(actor), "GET", &path, Value::Null, false).await;
    assert!(legacy["draft"]["senses"][0].get("relations").is_none());
    let mut legacy_draft = legacy["draft"].clone();
    legacy_draft["headword"] = json!("job");
    let (status, _) = request(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":3,"draft":legacy_draft}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, reloaded) = request(&state, Some(actor), "GET", &path, Value::Null, true).await;
    assert_eq!(reloaded["draft"]["senses"][0]["relations"], rows);
    let target_path = format!("/entries/{}", target["id"].as_str().unwrap());
    let mut removed = target["draft"].clone();
    removed["senses"] = json!([]);
    assert_eq!(
        request(
            &state,
            Some(actor),
            "PUT",
            &target_path,
            json!({"expected_revision":2,"draft":removed}),
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let archive = json!({"request_id":Uuid::now_v7(),"action":"archive","items":[
        {"entry_id":target["id"],"base_revision":2,"base_lifecycle_revision":1}
    ]});
    // Source's owner can archive with the own-resource permission.
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.archive',$1)").bind(actor).execute(&pool).await.unwrap();
    let (status, failure) = request(
        &state,
        Some(actor),
        "POST",
        "/workspace/commands",
        archive,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{failure}");
    let mut cleared = reloaded["draft"].clone();
    cleared["senses"][0]["relations"] = json!([]);
    let (status, _) = request(
        &state,
        Some(actor),
        "PUT",
        &path,
        json!({"expected_revision":4,"draft":cleared}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        request(
            &state,
            Some(actor),
            "PUT",
            &target_path,
            json!({"expected_revision":2,"draft":removed}),
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, caps) = request(
        &state,
        Some(actor),
        "GET",
        "/capabilities",
        Value::Null,
        true,
    )
    .await;
    assert_eq!(caps["relations"], true);
    let (_, caps) = request(
        &state,
        Some(actor),
        "GET",
        "/capabilities",
        Value::Null,
        false,
    )
    .await;
    assert!(caps.get("relations").is_none());
}

#[sqlx::test]
async fn relations_reject_foreign_targets_duplicate_ids_and_invalid_shapes(pool: PgPool) {
    let actor = admin(&pool, EDIT).await;
    let state = AppState::for_test(pool.clone());
    let source = entry(&state, actor, "zh-Hans").await;
    let foreign = entry(&state, actor, "zh-Hant").await;
    let path = format!("/entries/{}", source["id"].as_str().unwrap());
    let relation = json!({"id":Uuid::now_v7(),"relation":"synonym","pending_target_headword":"work","score":"80"});
    for patch in [
        json!({"target_word_id":foreign["id"],"target_sense_id":foreign["draft"]["senses"][0]["id"],"pending_target_headword":null}),
        json!({"target_word_id":source["id"],"target_sense_id":source["draft"]["senses"][0]["id"],"pending_target_headword":null}),
        json!({"target_word_id":Uuid::now_v7()}),
        json!({"score":"100.1"}),
        json!({"score":"1.001"}),
        json!({"relation":"unknown"}),
        json!({"id":source["draft"]["senses"][0]["id"]}),
    ] {
        let mut row = relation.clone();
        row.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let mut draft = source["draft"].clone();
        draft["senses"][0]["relations"] = json!([row]);
        let (status, response) = request(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":draft}),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    }
    let mut draft = source["draft"].clone();
    draft["senses"][0]["relations"] = json!([relation.clone(), {"id":Uuid::now_v7(),"relation":"synonym","pending_target_headword":"WORK","score":"60"}]);
    assert_eq!(
        request(
            &state,
            Some(actor),
            "PUT",
            &path,
            json!({"expected_revision":2,"draft":draft}),
            true
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
