use super::*;
use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[sqlx::test]
async fn completed_historical_answers_remain_visible_and_immutable(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 1).await;
    let (plan, _) = create_plan(&state, &auth, list, None).await;
    let run = open(&state, &auth, &plan).await;
    let page = questions(&state, &auth, &run).await;
    let input = answer_body(&page["items"][0], "apple");
    let answer = call(&state, &auth, "POST", &answer_path(&run), input.clone()).await;
    assert_eq!(answer.0, 200, "{}", answer.1);
    let run_id: Uuid = run["id"].as_str().unwrap().parse().unwrap();
    let before: (Uuid, String, bool, String) = sqlx::query_as("SELECT id,submitted_answer,is_correct,grading_version FROM learning_answers WHERE run_id=$1").bind(run_id).fetch_one(&pool).await.unwrap();
    // Model an already completed batch created before the Chinese-only generation rule.
    sqlx::query("UPDATE learning_questions SET prompt_snapshot=jsonb_set(prompt_snapshot,'{definition}','\"a round fruit\"') WHERE run_id=$1").bind(run_id).execute(&pool).await.unwrap();
    let history = questions(&state, &auth, &run).await;
    assert_eq!(history["items"][0]["prompt"]["definition"], "a round fruit");
    assert_eq!(history["items"][0]["feedback"]["is_correct"], true);
    let replay = call(&state, &auth, "POST", &answer_path(&run), input).await;
    assert_eq!(replay.0, 200, "{}", replay.1);
    assert_eq!(replay.1["answer_id"], answer.1["answer_id"]);
    let after: (Uuid, String, bool, String) = sqlx::query_as("SELECT id,submitted_answer,is_correct,grading_version FROM learning_answers WHERE run_id=$1").bind(run_id).fetch_one(&pool).await.unwrap();
    assert_eq!(after, before);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT prompt_snapshot->>'definition' FROM learning_questions WHERE run_id=$1"
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "a round fruit"
    );
}

#[sqlx::test]
async fn direct_practice_reuse_is_atomic_and_replay_survives_scope_changes(pool: PgPool) {
    let (state, auth, original) = fixture(&pool, 1).await;
    let entry: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(original)
            .fetch_one(&pool)
            .await
            .unwrap();
    let saved = call(&state, &auth, "POST", "/api/v1/me/wordlists", json!({"idempotency_key":Uuid::now_v7(),"name":"原子续练","items":[{"entry_id":entry,"private_note":""},{"item_id":Uuid::now_v7(),"custom_word":{"word":"teach","definition":"教；教授"},"private_note":""}]})).await;
    assert_eq!(saved.0, 200, "{}", saved.1);
    let list: Uuid = saved.1["id"].as_str().unwrap().parse().unwrap();
    let mut body = preview_body(list, None);
    body["practice_mode"] = json!("mastery_v1");
    let p = request(
        &state,
        &auth,
        "POST",
        "/api/v1/me/learning-plans/preview",
        body.clone(),
    )
    .await;
    assert_eq!(p.0, 200, "{}", p.1);
    body["reuse_active"] = json!(true);
    body["name"] = json!("原子续练");
    body["preview_fingerprint"] = p.1["preview_fingerprint"].clone();
    body["idempotency_key"] = json!(Uuid::now_v7());
    let mut second = body.clone();
    second["idempotency_key"] = json!(Uuid::now_v7());
    let (a, b) = tokio::join!(
        request(
            &state,
            &auth,
            "POST",
            "/api/v1/me/learning-plans",
            body.clone()
        ),
        request(
            &state,
            &auth,
            "POST",
            "/api/v1/me/learning-plans",
            second.clone()
        )
    );
    assert_eq!(a.0, 200, "{}", a.1);
    assert_eq!(b.0, 200, "{}", b.1);
    let task = a.1["task"]["id"].as_str().unwrap();
    assert_eq!(a.1["task"]["id"], b.1["task"]["id"]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_tasks")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let opened = request(
        &state,
        &auth,
        "POST",
        &format!("/api/v1/me/learning-plans/{task}/runs"),
        start_body(None),
    )
    .await;
    assert_eq!(opened.0, 200, "{}", opened.1);
    let run = opened.1;
    let page = request(
        &state,
        &auth,
        "GET",
        &format!(
            "/api/v1/me/learning-runs/{}/questions",
            run["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(page.1["items"].as_array().unwrap().len(), 2);
    let answered = request(
        &state,
        &auth,
        "POST",
        &answer_path(&run),
        answer_body(&page.1["items"][0], "apple"),
    )
    .await;
    assert_eq!(answered.0, 200, "{}", answered.1);
    let mut third = body.clone();
    third["idempotency_key"] = json!(Uuid::now_v7());
    let resumed = request(
        &state,
        &auth,
        "POST",
        "/api/v1/me/learning-plans",
        third.clone(),
    )
    .await;
    assert_eq!(resumed.0, 200, "{}", resumed.1);
    assert_eq!(resumed.1["current_run"]["id"], run["id"]);
    assert_eq!(resumed.1["current_run"]["answered_count"], 1);
    sqlx::query("UPDATE wordlists SET revision=revision+1 WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let replay = request(&state, &auth, "POST", "/api/v1/me/learning-plans", third).await;
    assert_eq!(replay.0, 200, "{}", replay.1);
    assert_eq!(replay.1["task"]["id"], a.1["task"]["id"]);
    let mut preview = preview_body(list, None);
    preview["practice_mode"] = json!("mastery_v1");
    let updated = request(
        &state,
        &auth,
        "POST",
        "/api/v1/me/learning-plans/preview",
        preview.clone(),
    )
    .await;
    preview["reuse_active"] = json!(true);
    preview["idempotency_key"] = json!(Uuid::now_v7());
    preview["name"] = json!("新范围");
    preview["preview_fingerprint"] = updated.1["preview_fingerprint"].clone();
    let new = request(&state, &auth, "POST", "/api/v1/me/learning-plans", preview).await;
    assert_eq!(new.0, 200, "{}", new.1);
    assert_ne!(new.1["task"]["id"], a.1["task"]["id"]);
    assert!(
        sqlx::raw_sql(include_str!(
            "../../migrations/20261102010000_practice_configuration.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_tasks")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}

#[sqlx::test]
async fn unused_practice_configuration_migration_roundtrips(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../../migrations/20261102010000_practice_configuration.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261102010000_practice_configuration.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_plan_reuse_receipts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

async fn request(
    state: &AppState,
    auth: &AuthUser,
    method: &str,
    path: &str,
    body: Value,
) -> (axum::http::StatusCode, Value) {
    let token = state
        .token_manager
        .generate_with_version(auth.subject, "student", 0)
        .unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .header(
                    "X-TSZ-Learning-Capabilities",
                    "choice-v1,mastery-v1,wordlist-spelling-v1,strict-choice-v1,practice-config-v1",
                )
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn mixed_plan(state: &AppState, auth: &AuthUser, list: Uuid) -> Value {
    let mut body = preview_body(list, None);
    body["question_type"] = json!("choice_zh_to_en");
    let p = request(
        state,
        auth,
        "POST",
        "/api/v1/me/learning-plans/preview",
        body.clone(),
    )
    .await;
    assert_eq!(p.0, 200, "{}", p.1);
    body["idempotency_key"] = json!(Uuid::now_v7());
    body["name"] = json!("双题型回归");
    body["preview_fingerprint"] = p.1["preview_fingerprint"].clone();
    let p = request(state, auth, "POST", "/api/v1/me/learning-plans", body).await;
    assert_eq!(p.0, 200, "{}", p.1);
    p.1
}

#[sqlx::test]
async fn mixed_spelling_answers_update_spelling_memory(pool: PgPool) {
    let (mut state, auth, list) = fixture(&pool, 1).await;
    state.learning_memory_enabled = true;
    sqlx::query("INSERT INTO learning_memory_enrollments(user_id,enabled) VALUES($1,true)")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    let plan = mixed_plan(&state, &auth, list).await;
    let opened = request(
        &state,
        &auth,
        "POST",
        &format!(
            "/api/v1/me/learning-plans/{}/runs",
            plan["task"]["id"].as_str().unwrap()
        ),
        start_body(None),
    )
    .await;
    assert_eq!(opened.0, 200, "{}", opened.1);
    let run = opened.1;
    let page = request(
        &state,
        &auth,
        "GET",
        &format!(
            "/api/v1/me/learning-runs/{}/questions",
            run["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert!(page.1["items"][0].get("options").is_none());
    let valid: bool = sqlx::query_scalar("SELECT NOT (answer_snapshot->>'wordlist_fallback')::boolean FROM learning_questions WHERE run_id=$1")
        .bind(run["id"].as_str().unwrap().parse::<Uuid>().unwrap()).fetch_one(&pool).await.unwrap();
    assert!(valid);
    let answered = request(
        &state,
        &auth,
        "POST",
        &answer_path(&run),
        answer_body(&page.1["items"][0], "apple"),
    )
    .await;
    assert_eq!(answered.0, 200, "{}", answered.1);
    assert_eq!(answered.1["is_correct"], true);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM learning_memory_events WHERE effect='initial_correct'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
}

#[sqlx::test]
async fn custom_questions_reject_reversed_languages_and_answer_leaks(pool: PgPool) {
    let (state, auth, _) = fixture(&pool, 1).await;
    let items = [("苹果", "apple"), ("苹果", "苹果"), ("apple", "苹果 apple"), ("teach", "教；教授")]
        .map(|(word, definition)| json!({"item_id":Uuid::now_v7(),"custom_word":{"word":word,"definition":definition},"private_note":""}));
    let saved = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        json!({"idempotency_key":Uuid::now_v7(),"name":"原文保留，学习校验","items":items}),
    )
    .await;
    assert_eq!(saved.0, 200, "{}", saved.1);
    let id: Uuid = saved.1["id"].as_str().unwrap().parse().unwrap();
    let preview = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/learning-plans/preview",
        preview_body(id, None),
    )
    .await;
    assert_eq!(preview.0, 200, "{}", preview.1);
    assert_eq!(preview.1["eligible_count"], 1, "{}", preview.1);
    assert_eq!(preview.1["exclusions"]["no_chinese_definition"], 1);
    assert_eq!(preview.1["exclusions"]["no_base_form"], 1);
    assert_eq!(preview.1["exclusions"]["answer_in_prompt"], 1);
    let (plan, _) = create_plan(&state, &auth, id, None).await;
    let run = open(&state, &auth, &plan).await;
    let page = questions(&state, &auth, &run).await;
    assert_eq!(page["items"][0]["prompt"]["definition"], "教；教授");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wordlist_items WHERE wordlist_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        4
    );
}

#[sqlx::test]
async fn english_levels_do_not_displace_eligible_chinese_definitions(pool: PgPool) {
    use tsz_rust::learning_tasks::{
        dto::LearningContext,
        question::{LearningSource, build_candidates, build_wordlist_candidates},
    };
    let (_, _, list) = fixture(&pool, 1).await;
    let (entry, publication, mut snapshot): (Uuid, Uuid, Value) = sqlx::query_as("SELECT e.id,p.id,p.snapshot FROM wordlist_items i JOIN lexicon.entries e ON e.id=i.entry_id JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE i.wordlist_id=$1")
        .bind(list).fetch_one(&pool).await.unwrap();
    let mut english = snapshot["meanings"]["pos"][0]["senses"][0]["definitions"][0].clone();
    english.as_object_mut().unwrap().remove("content_id");
    english["id"] = json!(Uuid::now_v7());
    english["definition_mode"] = json!("en_definition");
    english["level"] = json!("A2");
    english["content"] = json!({"mode":"unified","common":{"id":Uuid::now_v7(),"origin":"manual","value":{"version":1,"text":"a round fruit","spans":[],"liaisons":[]}}});
    snapshot["meanings"]["pos"][0]["senses"][0]["definitions"]
        .as_array_mut()
        .unwrap()
        .push(english);
    let word: tsz_rust::lexicon::dto::AdminWordV3 = serde_json::from_value(snapshot).unwrap();
    let source = LearningSource {
        wordlist_id: list,
        revision: 1,
        membership_id: Uuid::now_v7(),
        public_generation: None,
        entry_id: entry,
        archive_generation: 1,
        publication_id: publication,
        content: tsz_rust::lexicon::published::PublishedContentV3 {
            forms: word.forms,
            meanings: word.meanings,
        },
    };
    let context = LearningContext {
        cefr_level: "A2".into(),
        english_variant: "BrE".into(),
    };
    let sources = [source];
    assert_eq!(
        build_candidates(&sources, &context)
            .unwrap()
            .candidates
            .len(),
        1
    );
    assert!(
        !build_wordlist_candidates(&sources, &context)
            .unwrap()
            .candidates[0]
            .answer
            .wordlist_fallback
    );
}

#[sqlx::test]
async fn platform_wordlist_questions_do_not_reveal_the_english_answer(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 1).await;
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=jsonb_set(snapshot,'{meanings,pos,0,senses,0,definitions,0,content,text}','\"苹果 apple\"') WHERE id=(SELECT e.current_publication_id FROM wordlist_items i JOIN lexicon.entries e ON e.id=i.entry_id WHERE i.wordlist_id=$1)")
        .bind(list).execute(&pool).await.unwrap();
    let p = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/learning-plans/preview",
        preview_body(list, None),
    )
    .await;
    assert_eq!(p.0, 200, "{}", p.1);
    assert_eq!(p.1["eligible_count"], 0);
    assert_eq!(p.1["exclusions"]["answer_in_prompt"], 1);
}

#[sqlx::test]
async fn previously_queued_english_prompts_cannot_create_new_questions(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 1).await;
    let (plan, _) = create_plan(&state, &auth, list, None).await;
    let cycle: Uuid = plan["current_cycle"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query("UPDATE learning_cycle_units SET prompt_snapshot=jsonb_set(prompt_snapshot,'{definition}','\"a round fruit\"') WHERE cycle_id=$1").bind(cycle).execute(&pool).await.unwrap();
    let response = call(
        &state,
        &auth,
        "POST",
        &format!(
            "/api/v1/me/learning-plans/{}/runs",
            plan["task"]["id"].as_str().unwrap()
        ),
        start_body(None),
    )
    .await;
    assert_eq!(response.0, 409, "{}", response.1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_questions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT prompt_snapshot->>'definition' FROM learning_cycle_units WHERE cycle_id=$1"
        )
        .bind(cycle)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "a round fruit"
    );
}

#[sqlx::test]
async fn old_invalid_unanswered_content_is_hidden_without_rewriting_history(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 1).await;
    let (plan, _) = create_plan(&state, &auth, list, None).await;
    let run = open(&state, &auth, &plan).await;
    let run_id: Uuid = run["id"].as_str().unwrap().parse().unwrap();
    sqlx::query("UPDATE learning_questions SET prompt_snapshot=jsonb_set(prompt_snapshot,'{definition}','\"a round fruit\"') WHERE run_id=$1").bind(run_id).execute(&pool).await.unwrap();
    let page = questions(&state, &auth, &run).await;
    assert_eq!(page["items"][0]["content_available"], false);
    assert!(page["items"][0]["prompt"].is_null());
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &answer_path(&run),
            answer_body(&page["items"][0], "apple")
        )
        .await
        .0,
        409
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT prompt_snapshot->>'definition' FROM learning_questions WHERE run_id=$1"
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "a round fruit"
    );
}
