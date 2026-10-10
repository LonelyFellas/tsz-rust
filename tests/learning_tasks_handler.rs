mod account_deletion_support;
mod learning_tasks_support;
mod wordlists_support;
use account_deletion_support::call;
use learning_tasks_support::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
#[sqlx::test]
async fn eligibility_unknown_fields_preview_and_receipts(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let mut input = create_body(list, "daily");
    let mut preview = input.clone();
    preview.as_object_mut().unwrap().remove("name");
    preview.as_object_mut().unwrap().remove("idempotency_key");
    let p = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/learning-tasks/preview",
        preview.clone(),
    )
    .await;
    assert_eq!(p.0, 200, "{}", p.1);
    assert_eq!(p.1["eligible_count"], 2);
    assert_eq!(p.1["can_start"], true);
    preview["daily_question_count"] = json!(3);
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/learning-tasks/preview",
            preview
        )
        .await
        .1["can_start"],
        false
    );
    let (one, two) = tokio::join!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/learning-tasks",
            input.clone()
        ),
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/learning-tasks",
            input.clone()
        )
    );
    assert_eq!(one.0, 200, "{}", one.1);
    assert_eq!(one, two);
    input["name"] = json!("changed");
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/learning-tasks",
            input.clone()
        )
        .await
        .0,
        409
    );
    input["idempotency_key"] = json!(Uuid::now_v7());
    input["completed"] = json!(true);
    assert_eq!(
        call(&state, &auth, "POST", "/api/v1/me/learning-tasks", input)
            .await
            .0,
        422
    );
    let (_, other, _) = setup(&pool).await;
    assert_eq!(
        call(
            &state,
            &other,
            "GET",
            &format!(
                "/api/v1/me/learning-tasks/{}",
                one.1["id"].as_str().unwrap()
            ),
            Value::Null
        )
        .await
        .0,
        404
    );
    assert_eq!(
        call(
            &state,
            &other,
            "POST",
            "/api/v1/me/learning-tasks",
            create_body(list, "daily")
        )
        .await
        .0,
        409
    );
    sqlx::query("DELETE FROM student_profiles WHERE user_id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            "/api/v1/me/learning-tasks",
            Value::Null
        )
        .await
        .1["code"],
        "learning_settings_required"
    );
    sqlx::query("DELETE FROM user_roles WHERE user_id=$1 AND role='student'")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            "/api/v1/me/learning-tasks",
            Value::Null
        )
        .await
        .0,
        403
    );
}
#[sqlx::test]
async fn first_answers_all_wrong_complete_once_and_hide_unanswered(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "daily").await;
    let run = start(&state, &auth, &task).await;
    let qs = questions(&state, &auth, &run).await;
    assert!(!qs.to_string().contains("apple"));
    assert!(qs["items"][0]["feedback"].is_null());
    let path = answer_path(&run);
    let bad = answer_body(&qs["items"][0], " ");
    assert_eq!(call(&state, &auth, "POST", &path, bad).await.0, 400);
    let first = answer_body(&qs["items"][0], "wrong");
    let (a, b) = tokio::join!(
        call(&state, &auth, "POST", &path, first.clone()),
        call(&state, &auth, "POST", &path, first.clone())
    );
    assert_eq!(a.0, 200, "{}", a.1);
    assert_eq!(b.0, 200);
    assert_eq!(a.1["answer_id"], b.1["answer_id"]);
    assert_eq!(a.1["run"]["answered_count"], 1);
    assert!(a.1["run"]["completion_id"].is_null());
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &path,
            answer_body(&qs["items"][0], "apple")
        )
        .await
        .0,
        409
    );
    let mut changed = first.clone();
    changed["answer"] = json!("apple");
    assert_eq!(call(&state, &auth, "POST", &path, changed).await.0, 409);
    let last = answer_body(&qs["items"][1], "also wrong");
    let (a, b) = tokio::join!(
        call(&state, &auth, "POST", &path, last.clone()),
        call(&state, &auth, "POST", &path, last)
    );
    assert_eq!(a.0, 200, "{}", a.1);
    assert_eq!(b.0, 200);
    assert_eq!(a.1["run"]["state"], "completed");
    assert_eq!(a.1["run"]["correct_count"], 0);
    assert_eq!(a.1["run"]["completion_id"], b.1["run"]["completion_id"]);
    sqlx::query("DELETE FROM wordlists WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let again = start(&state, &auth, &task).await;
    assert_eq!(again["id"], run["id"]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_completions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM coin_operations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn candidate_projection_obeys_cefr_context_and_form_relationships(pool: PgPool) {
    use tsz_rust::learning_tasks::{
        dto::LearningContext,
        question::{LearningSource, build_candidates},
    };
    let (_, _, list) = setup(&pool).await;
    let (entry,publication,generation,snapshot):(Uuid,Uuid,i64,Value)=sqlx::query_as("SELECT e.id,p.id,e.wordlist_archive_generation,p.snapshot FROM wordlist_items i JOIN lexicon.entries e ON e.id=i.entry_id JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE i.wordlist_id=$1").bind(list).fetch_one(&pool).await.unwrap();
    let source = |snapshot: Value| {
        let word: tsz_rust::lexicon::dto::AdminWordV3 = serde_json::from_value(snapshot).unwrap();
        LearningSource {
            wordlist_id: list,
            revision: 1,
            membership_id: Uuid::now_v7(),
            public_generation: None,
            entry_id: entry,
            archive_generation: generation,
            publication_id: publication,
            content: tsz_rust::lexicon::published::PublishedContentV3 {
                forms: word.forms,
                meanings: word.meanings,
            },
        }
    };
    let context = LearningContext {
        cefr_level: "A1".into(),
        english_variant: "BrE".into(),
    };
    let mut s = snapshot.clone();
    s["forms"]["pos"][0]["forms"][0]["regional_variants"]["us"]["spelling"] = json!("us-only");
    let a = build_candidates(&[source(s.clone()), source(s.clone())], &context).unwrap();
    assert_eq!(a.candidates.len(), 2);
    assert!(a.candidates[0].answer.answers.contains(&"apple".into()));
    assert!(!a.candidates[0].answer.answers.contains(&"us-only".into()));
    let us = build_candidates(
        &[source(s.clone())],
        &LearningContext {
            cefr_level: "A1".into(),
            english_variant: "AmE".into(),
        },
    )
    .unwrap();
    assert!(us.candidates[0].answer.answers.contains(&"us-only".into()));
    s["meanings"]["pos"][0]["senses"][0]["depends_on_context"] = json!(true);
    assert_eq!(
        build_candidates(&[source(s.clone())], &context)
            .unwrap()
            .exclusions
            .context_dependent,
        1
    );
    let mut s = snapshot.clone();
    let mut sentence = s["meanings"]["pos"][0]["senses"][0]["definitions"][0].clone();
    sentence["definition_mode"] = json!("zh_sentence");
    s["meanings"]["pos"][0]["senses"][0]["definitions"]
        .as_array_mut()
        .unwrap()
        .insert(0, sentence);
    assert_eq!(
        build_candidates(&[source(s)], &context)
            .unwrap()
            .exclusions
            .no_chinese_definition,
        1
    );
    let mut s = snapshot.clone();
    s["meanings"]["pos"][0]["senses"][0]["definitions"][0]["level"] = json!("A2");
    assert_eq!(
        build_candidates(&[source(s)], &context)
            .unwrap()
            .candidates
            .len(),
        1
    );
    let mut s = snapshot;
    s["forms"]["pos"][0]["form_groups"][0]["scope"] = json!("dedicated");
    assert_eq!(
        build_candidates(&[source(s)], &context)
            .unwrap()
            .exclusions
            .no_base_form,
        2
    );
}
