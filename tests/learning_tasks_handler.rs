mod account_deletion_support;
mod learning_tasks_support;
mod wordlists_support;
use account_deletion_support::call;
use learning_tasks_support::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
#[sqlx::test]
async fn archive_impact_is_private_aggregate_deduplicated_and_revision_checked(pool: PgPool) {
    use wordlists_support::{admin, admin_call};
    let (state, auth, list) = setup(&pool).await;
    let state = state
        .with_smart_lexicon_v3_flags_for_test(tsz_rust::config::SmartLexiconV3Flags::all_enabled());
    let task = create(&state, &auth, list, "daily").await;
    let run = start(&state, &auth, &task).await;
    let entry: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    let actor = admin(&pool).await;
    let path = "/api/v1/admin/lexicon/entries/archive-impact";
    let target = json!({"id":entry,"base_revision":1,"base_lifecycle_revision":1});
    let body = json!({"entries":[target.clone()]});
    let impact = admin_call(&state, actor, "POST", path, body.clone()).await;
    assert_eq!(impact.0, 200, "{}", impact.1);
    assert_eq!(impact.1, json!({"wordlist_count":1,"active_run_count":1}));
    let second = wordlists_support::entry(&pool, "batch-second").await;
    let updated = call(&state, &auth, "PUT", &format!("/api/v1/me/wordlists/{list}"), json!({"expected_revision":1,"content":{"name":"batch impact","entry_ids":[entry,second]},"note_updates":[]})).await;
    assert_eq!(updated.0, 200, "{}", updated.1);
    let batch = admin_call(&state, actor, "POST", path, json!({"entries":[target.clone(),{"id":second,"base_revision":1,"base_lifecycle_revision":1}]})).await;
    assert_eq!(batch.0, 200, "{}", batch.1);
    assert_eq!(batch.1, json!({"wordlist_count":1,"active_run_count":1}));
    assert_eq!(
        admin_call(&state, actor, "POST", path, json!({"entries":[]}))
            .await
            .0,
        422
    );
    assert_eq!(
        admin_call(
            &state,
            actor,
            "POST",
            path,
            json!({"entries":[target.clone(),target.clone()]})
        )
        .await
        .0,
        422
    );
    let mut stale = body.clone();
    stale["entries"][0]["base_revision"] = json!(9);
    assert_eq!(admin_call(&state, actor, "POST", path, stale).await.0, 409);
    let mut stale = body.clone();
    stale["entries"][0]["base_lifecycle_revision"] = json!(9);
    assert_eq!(admin_call(&state, actor, "POST", path, stale).await.0, 409);
    sqlx::query(
        "UPDATE learning_runs SET expires_at=clock_timestamp()-interval '1 second' WHERE id=$1",
    )
    .bind(Uuid::parse_str(run["id"].as_str().unwrap()).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        admin_call(&state, actor, "POST", path, body.clone())
            .await
            .1,
        json!({"wordlist_count":1,"active_run_count":0})
    );
    sqlx::query("UPDATE wordlists SET deleted_at=clock_timestamp() WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        admin_call(&state, actor, "POST", path, body.clone())
            .await
            .1,
        json!({"wordlist_count":0,"active_run_count":0})
    );
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(admin_call(&state, actor, "POST", path, body).await.0, 403);
}

#[sqlx::test]
async fn archive_impact_excludes_runs_with_unavailable_non_target_sources(pool: PgPool) {
    use wordlists_support::{admin, admin_call};
    let (state, auth, list) = setup(&pool).await;
    let state = state
        .with_smart_lexicon_v3_flags_for_test(tsz_rust::config::SmartLexiconV3Flags::all_enabled());
    let entry: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    let second = wordlists_support::full_entry(&pool, "banana").await;
    let updated = call(
        &state,
        &auth,
        "PUT",
        &format!("/api/v1/me/wordlists/{list}"),
        json!({"expected_revision":1,"content":{"name":"whole run impact","entry_ids":[entry,second]},"note_updates":[]}),
    )
    .await;
    assert_eq!(updated.0, 200, "{}", updated.1);
    let mut input = create_body(list, "daily");
    input["daily_question_count"] = json!(4);
    let task = call(&state, &auth, "POST", "/api/v1/me/learning-tasks", input).await;
    assert_eq!(task.0, 200, "{}", task.1);
    let run = call(
        &state,
        &auth,
        "POST",
        &format!(
            "/api/v1/me/learning-tasks/{}/runs",
            task.1["id"].as_str().unwrap()
        ),
        start_body(None),
    )
    .await;
    assert_eq!(run.0, 200, "{}", run.1);
    let run_id = Uuid::parse_str(run.1["id"].as_str().unwrap()).unwrap();
    let sources: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT entry_id FROM learning_questions WHERE run_id=$1 ORDER BY entry_id",
    )
    .bind(run_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let mut expected_sources = vec![entry, second];
    expected_sources.sort();
    assert_eq!(sources, expected_sources);
    let actor = admin(&pool).await;
    let path = "/api/v1/admin/lexicon/entries/archive-impact";
    let body = json!({"entries":[{"id":entry,"base_revision":1,"base_lifecycle_revision":1}]});
    let impact = admin_call(&state, actor, "POST", path, body.clone()).await;
    assert_eq!(impact.0, 200, "{}", impact.1);
    assert_eq!(impact.1, json!({"wordlist_count":1,"active_run_count":1}));

    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=$1")
        .bind(second)
        .execute(&pool)
        .await
        .unwrap();
    let run_path = format!("/api/v1/me/learning-runs/{run_id}");
    let invalidated = call(&state, &auth, "GET", &run_path, Value::Null).await;
    assert_eq!(invalidated.0, 200, "{}", invalidated.1);
    assert_eq!(invalidated.1["state"], "invalidated");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM learning_runs WHERE id=$1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "active"
    );
    let impact = admin_call(&state, actor, "POST", path, body.clone()).await;
    assert_eq!(impact.0, 200, "{}", impact.1);
    assert_eq!(impact.1, json!({"wordlist_count":1,"active_run_count":0}));

    sqlx::query("UPDATE lexicon.entries SET archived_at=NULL WHERE id=$1")
        .bind(second)
        .execute(&pool)
        .await
        .unwrap();
    let generation_matches: bool = sqlx::query_scalar(
        "SELECT bool_and(q.entry_archive_generation=e.wordlist_archive_generation) FROM learning_questions q JOIN lexicon.entries e ON e.id=q.entry_id WHERE q.run_id=$1 AND q.entry_id=$2",
    )
    .bind(run_id)
    .bind(second)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!generation_matches);
    let impact = admin_call(&state, actor, "POST", path, body.clone()).await;
    assert_eq!(impact.0, 200, "{}", impact.1);
    assert_eq!(impact.1, json!({"wordlist_count":1,"active_run_count":0}));

    let mut input = create_body(list, "daily");
    input["daily_question_count"] = json!(4);
    let task = call(&state, &auth, "POST", "/api/v1/me/learning-tasks", input).await;
    assert_eq!(task.0, 200, "{}", task.1);
    let fresh = call(
        &state,
        &auth,
        "POST",
        &format!(
            "/api/v1/me/learning-tasks/{}/runs",
            task.1["id"].as_str().unwrap()
        ),
        start_body(None),
    )
    .await;
    assert_eq!(fresh.0, 200, "{}", fresh.1);
    assert_ne!(fresh.1["id"], run.1["id"]);
    let impact = admin_call(&state, actor, "POST", path, body.clone()).await;
    assert_eq!(impact.0, 200, "{}", impact.1);
    assert_eq!(impact.1, json!({"wordlist_count":1,"active_run_count":1}));

    sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1 AND entry_id=$2")
        .bind(list)
        .bind(second)
        .execute(&pool)
        .await
        .unwrap();
    let impact = admin_call(&state, actor, "POST", path, body).await;
    assert_eq!(impact.0, 200, "{}", impact.1);
    assert_eq!(impact.1, json!({"wordlist_count":1,"active_run_count":0}));
}

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

async fn borrower(pool: &PgPool) -> (tsz_rust::state::AppState, tsz_rust::auth::extract::AuthUser) {
    let (state, auth) = account_deletion_support::setup_bound(pool).await;
    sqlx::query(
        "INSERT INTO student_profiles(user_id,cefr_level,english_variant) VALUES($1,'A1','BrE')",
    )
    .bind(auth.subject)
    .execute(pool)
    .await
    .unwrap();
    (state, auth)
}

async fn publish_list(pool: &PgPool, list: Uuid) {
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(list)
        .execute(pool)
        .await
        .unwrap();
}

async fn admin_delete(
    state: &tsz_rust::state::AppState,
    admin: Uuid,
    list: Uuid,
) -> (axum::http::StatusCode, Value) {
    use wordlists_support::admin_call;
    admin_call(
        state,
        admin,
        "POST",
        &format!("/api/v1/admin/wordlists/{list}/delete"),
        json!({"expected_revision":1}),
    )
    .await
}

async fn complete_run(
    state: &tsz_rust::state::AppState,
    auth: &tsz_rust::auth::extract::AuthUser,
    run: &Value,
) {
    let qs = questions(state, auth, run).await;
    for q in qs["items"].as_array().unwrap() {
        let r = call(
            state,
            auth,
            "POST",
            &answer_path(run),
            answer_body(q, "definitely-wrong"),
        )
        .await;
        assert_eq!(r.0, 200, "{}", r.1);
    }
}

#[sqlx::test]
async fn deleted_wordlist_lets_started_run_finish_and_archives_task(pool: PgPool) {
    use wordlists_support::{admin, admin_call};
    let (state, _owner, list) = setup(&pool).await;
    let (borrower_state, borrower_auth) = borrower(&pool).await;
    let actor = admin(&pool).await;
    publish_list(&pool, list).await;
    let task = create(&borrower_state, &borrower_auth, list, "longterm").await;
    let run = start(&borrower_state, &borrower_auth, &task).await;
    let blocked = create(&borrower_state, &borrower_auth, list, "daily").await;

    let impact = admin_call(
        &state,
        actor,
        "GET",
        &format!("/api/v1/admin/wordlists/{list}/delete-impact"),
        Value::Null,
    )
    .await;
    assert_eq!(impact.0, 200, "{}", impact.1);
    assert_eq!(
        impact.1,
        json!({
            "active_run_count": 1,
            "active_task_count": 1,
            "affected_user_count": 1,
            "blocked_task_count": 1
        })
    );

    let deleted = admin_delete(&state, actor, list).await;
    assert_eq!(deleted.0, 204, "{}", deleted.1);

    let notice = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        "/api/v1/me/notifications",
        Value::Null,
    )
    .await;
    assert_eq!(notice.0, 200, "{}", notice.1);
    assert_eq!(notice.1["items"][0]["kind"], "wordlist_deleted");
    assert!(
        notice.1["items"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("已被删除")
    );

    let run_path = format!("/api/v1/me/learning-runs/{}", run["id"].as_str().unwrap());
    let current = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &run_path,
        Value::Null,
    )
    .await;
    assert_eq!(current.0, 200, "{}", current.1);
    assert_eq!(current.1["state"], "active");
    let qs = questions(&borrower_state, &borrower_auth, &run).await;
    assert!(
        qs["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["content_available"] == true && q["prompt"].is_object())
    );

    complete_run(&borrower_state, &borrower_auth, &run).await;

    let detail = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &format!("/api/v1/me/learning-tasks/{}", task["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(detail.0, 200, "{}", detail.1);
    assert_eq!(detail.1["task"]["state"], "archived");
    let revision = detail.1["task"]["revision"].as_i64().unwrap();
    let again = call(
        &borrower_state,
        &borrower_auth,
        "POST",
        &format!(
            "/api/v1/me/learning-tasks/{}/runs",
            task["id"].as_str().unwrap()
        ),
        json!({"idempotency_key":Uuid::now_v7(),"expected_revision":revision,"after_run_id":run["id"]}),
    )
    .await;
    assert_eq!(again.0, 409, "{}", again.1);

    let blocked_start = call(
        &borrower_state,
        &borrower_auth,
        "POST",
        &format!(
            "/api/v1/me/learning-tasks/{}/runs",
            blocked["id"].as_str().unwrap()
        ),
        start_body(None),
    )
    .await;
    assert_eq!(blocked_start.0, 409, "{}", blocked_start.1);

    let created = call(
        &borrower_state,
        &borrower_auth,
        "POST",
        "/api/v1/me/learning-tasks",
        create_body(list, "daily"),
    )
    .await;
    assert_eq!(created.0, 409, "{}", created.1);
}

#[sqlx::test]
async fn completed_before_deletion_gets_no_grace(pool: PgPool) {
    use wordlists_support::admin;
    let (state, _owner, list) = setup(&pool).await;
    let (borrower_state, borrower_auth) = borrower(&pool).await;
    let actor = admin(&pool).await;
    publish_list(&pool, list).await;
    let task = create(&borrower_state, &borrower_auth, list, "longterm").await;
    let run = start(&borrower_state, &borrower_auth, &task).await;
    complete_run(&borrower_state, &borrower_auth, &run).await;
    assert_eq!(admin_delete(&state, actor, list).await.0, 204);

    let run_path = format!("/api/v1/me/learning-runs/{}", run["id"].as_str().unwrap());
    let current = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &run_path,
        Value::Null,
    )
    .await;
    assert_eq!(current.1["state"], "completed");
    let qs = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &format!("{run_path}/questions"),
        Value::Null,
    )
    .await;
    assert_eq!(qs.0, 200, "{}", qs.1);
    assert!(
        qs.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["content_available"] == false && q["prompt"].is_null())
    );

    let again = call(
        &borrower_state,
        &borrower_auth,
        "POST",
        &format!(
            "/api/v1/me/learning-tasks/{}/runs",
            task["id"].as_str().unwrap()
        ),
        json!({"idempotency_key":Uuid::now_v7(),"expected_revision":1,"after_run_id":run["id"]}),
    )
    .await;
    assert_eq!(again.0, 409, "{}", again.1);
    let created = call(
        &borrower_state,
        &borrower_auth,
        "POST",
        "/api/v1/me/learning-tasks",
        create_body(list, "daily"),
    )
    .await;
    assert_eq!(created.0, 409, "{}", created.1);
}

#[sqlx::test]
async fn grace_survives_restore_and_expires(pool: PgPool) {
    use wordlists_support::admin;
    let (state, _owner, list) = setup(&pool).await;
    let (borrower_state, borrower_auth) = borrower(&pool).await;
    let actor = admin(&pool).await;
    publish_list(&pool, list).await;
    let task = create(&borrower_state, &borrower_auth, list, "longterm").await;
    let run = start(&borrower_state, &borrower_auth, &task).await;
    assert_eq!(admin_delete(&state, actor, list).await.0, 204);

    // Restore as private; the deletion event must keep the in-flight run alive.
    sqlx::query("UPDATE wordlists SET deleted_at=NULL,state='draft' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let run_path = format!("/api/v1/me/learning-runs/{}", run["id"].as_str().unwrap());
    let active = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &run_path,
        Value::Null,
    )
    .await;
    assert_eq!(active.1["state"], "active", "{}", active.1);

    sqlx::query(
        "UPDATE wordlist_deletion_events SET deleted_at=r.started_at+interval '1 microsecond',grace_until=r.started_at+interval '2 microseconds' FROM learning_runs r WHERE wordlist_id=$1 AND r.id=$2",
    )
    .bind(list)
    .bind(Uuid::parse_str(run["id"].as_str().unwrap()).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let expired = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &run_path,
        Value::Null,
    )
    .await;
    assert_eq!(expired.1["state"], "invalidated", "{}", expired.1);
    let qs = questions(&borrower_state, &borrower_auth, &run).await;
    let answered = call(
        &borrower_state,
        &borrower_auth,
        "POST",
        &answer_path(&run),
        answer_body(&qs["items"][0], "definitely-wrong"),
    )
    .await;
    assert_eq!(answered.0, 409, "{}", answered.1);
}

#[sqlx::test]
async fn entry_archive_still_invalidates_grace(pool: PgPool) {
    use wordlists_support::admin;
    let (state, _owner, list) = setup(&pool).await;
    let (borrower_state, borrower_auth) = borrower(&pool).await;
    let actor = admin(&pool).await;
    publish_list(&pool, list).await;
    let task = create(&borrower_state, &borrower_auth, list, "longterm").await;
    let run = start(&borrower_state, &borrower_auth, &task).await;
    assert_eq!(admin_delete(&state, actor, list).await.0, 204);
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=(SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1)")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let current = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &format!("/api/v1/me/learning-runs/{}", run["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(current.1["state"], "invalidated", "{}", current.1);
}

#[sqlx::test]
async fn owner_trash_also_grants_grace(pool: PgPool) {
    let (state, owner, list) = setup(&pool).await;
    let (borrower_state, borrower_auth) = borrower(&pool).await;
    publish_list(&pool, list).await;
    let task = create(&borrower_state, &borrower_auth, list, "longterm").await;
    let run = start(&borrower_state, &borrower_auth, &task).await;

    let library = call(
        &state,
        &owner,
        "GET",
        "/api/v1/me/wordlist-library/owned",
        Value::Null,
    )
    .await;
    assert_eq!(library.0, 200, "{}", library.1);
    let node = library.1["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["wordlist_id"] == list.to_string())
        .unwrap()["id"]
        .clone();
    let trashed = call(
        &state,
        &owner,
        "POST",
        "/api/v1/me/wordlist-library/owned/operations",
        json!({
            "expected_version": library.1["version"],
            "action": "trash",
            "ids": [node]
        }),
    )
    .await;
    assert_eq!(trashed.0, 200, "{}", trashed.1);

    let current = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &format!("/api/v1/me/learning-runs/{}", run["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(current.1["state"], "active", "{}", current.1);

    // Permanent deletion removes wordlist_items; frozen questions must still finish.
    let trash = call(
        &state,
        &owner,
        "GET",
        "/api/v1/me/wordlist-library/owned?trash=true",
        Value::Null,
    )
    .await;
    assert_eq!(trash.0, 200, "{}", trash.1);
    let trashed = trash.1["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["wordlist_id"] == list.to_string())
        .unwrap()["id"]
        .clone();
    let purged = call(
        &state,
        &owner,
        "POST",
        "/api/v1/me/wordlist-library/owned/operations",
        json!({
            "expected_version": trash.1["version"],
            "action": "purge",
            "ids": [trashed]
        }),
    )
    .await;
    assert_eq!(purged.0, 200, "{}", purged.1);
    let still = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &format!("/api/v1/me/learning-runs/{}", run["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(still.1["state"], "active", "{}", still.1);
    complete_run(&borrower_state, &borrower_auth, &run).await;
}

#[sqlx::test]
async fn uncommitted_delete_cannot_invalidate_a_waiting_run(pool: PgPool) {
    let (_state, _owner, list) = setup(&pool).await;
    let (borrower_state, borrower_auth) = borrower(&pool).await;
    publish_list(&pool, list).await;
    let task = create(&borrower_state, &borrower_auth, list, "longterm").await;
    let run = start(&borrower_state, &borrower_auth, &task).await;
    let qs = questions(&borrower_state, &borrower_auth, &run).await;
    let body = answer_body(&qs["items"][0], "definitely-wrong");

    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM wordlists WHERE id=$1 FOR UPDATE")
        .bind(list)
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let deleted_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO wordlist_deletion_events(id,wordlist_id,deleted_at,grace_until,prior_state,public_generation) VALUES($1,$2,$3,$3+interval '7 days','published',0)",
    )
    .bind(Uuid::now_v7())
    .bind(list)
    .bind(deleted_at)
    .execute(&mut *blocker)
    .await
    .unwrap();
    sqlx::query("UPDATE wordlists SET deleted_at=$2,state='draft' WHERE id=$1")
        .bind(list)
        .bind(deleted_at)
        .execute(&mut *blocker)
        .await
        .unwrap();

    let s = borrower_state.clone();
    let auth = tsz_rust::auth::extract::AuthUser {
        subject: borrower_auth.subject,
        role: "student".into(),
        security_version: borrower_auth.security_version,
    };
    let path = answer_path(&run);
    let answer = tokio::spawn(async move { call(&s, &auth, "POST", &path, body).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT id,owner_user_id,state,revision,learning_public_generation%')",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    blocker.commit().await.unwrap();
    let result = answer.await.unwrap();
    assert_eq!(result.0, 200, "{}", result.1);
}

#[sqlx::test]
async fn withdraw_then_delete_does_not_revive_an_invalidated_run(pool: PgPool) {
    let (state, owner, list) = setup(&pool).await;
    let (borrower_state, borrower_auth) = borrower(&pool).await;
    publish_list(&pool, list).await;
    let task = create(&borrower_state, &borrower_auth, list, "longterm").await;
    let run = start(&borrower_state, &borrower_auth, &task).await;

    let withdrawn = call(
        &state,
        &owner,
        "POST",
        &format!("/api/v1/me/wordlists/{list}/withdraw"),
        json!({"expected_revision":1}),
    )
    .await;
    assert_eq!(withdrawn.0, 200, "{}", withdrawn.1);
    let run_path = format!("/api/v1/me/learning-runs/{}", run["id"].as_str().unwrap());
    let invalidated = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &run_path,
        Value::Null,
    )
    .await;
    assert_eq!(invalidated.1["state"], "invalidated", "{}", invalidated.1);

    let library = call(
        &state,
        &owner,
        "GET",
        "/api/v1/me/wordlist-library/owned",
        Value::Null,
    )
    .await;
    assert_eq!(library.0, 200, "{}", library.1);
    let node = library.1["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["wordlist_id"] == list.to_string())
        .unwrap()["id"]
        .clone();
    let trashed = call(
        &state,
        &owner,
        "POST",
        "/api/v1/me/wordlist-library/owned/operations",
        json!({
            "expected_version": library.1["version"],
            "action": "trash",
            "ids": [node]
        }),
    )
    .await;
    assert_eq!(trashed.0, 200, "{}", trashed.1);

    let still = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &run_path,
        Value::Null,
    )
    .await;
    assert_eq!(still.1["state"], "invalidated", "{}", still.1);
    let qs = questions(&borrower_state, &borrower_auth, &run).await;
    let answered = call(
        &borrower_state,
        &borrower_auth,
        "POST",
        &answer_path(&run),
        answer_body(&qs["items"][0], "definitely-wrong"),
    )
    .await;
    assert_eq!(answered.0, 409, "{}", answered.1);
    let notice = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        "/api/v1/me/notifications",
        Value::Null,
    )
    .await;
    assert_eq!(notice.0, 200, "{}", notice.1);
    assert_eq!(notice.1["items"], json!([]));
}

#[sqlx::test]
async fn restore_then_governance_withdraw_ends_existing_grace(pool: PgPool) {
    use wordlists_support::{admin, admin_call};
    let (state, _owner, list) = setup(&pool).await;
    let (borrower_state, borrower_auth) = borrower(&pool).await;
    let actor = admin(&pool).await;
    publish_list(&pool, list).await;
    let task = create(&borrower_state, &borrower_auth, list, "longterm").await;
    let run = start(&borrower_state, &borrower_auth, &task).await;
    assert_eq!(admin_delete(&state, actor, list).await.0, 204);

    sqlx::query("UPDATE wordlists SET deleted_at=NULL,state='published' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let detail = admin_call(
        &state,
        actor,
        "GET",
        &format!("/api/v1/admin/wordlists/{list}"),
        Value::Null,
    )
    .await;
    assert_eq!(detail.0, 200, "{}", detail.1);
    let revision = detail.1["wordlist"]["revision"].as_i64().unwrap();
    let withdrawn = admin_call(
        &state,
        actor,
        "POST",
        &format!("/api/v1/admin/wordlists/{list}/withdraw"),
        json!({"expected_revision":revision,"reason":"治理下架"}),
    )
    .await;
    assert_eq!(withdrawn.0, 200, "{}", withdrawn.1);

    let current = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &format!("/api/v1/me/learning-runs/{}", run["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(current.1["state"], "invalidated", "{}", current.1);
}

#[sqlx::test]
async fn delete_impact_does_not_promise_grace_for_a_non_public_wordlist(pool: PgPool) {
    use wordlists_support::{admin, admin_call};
    let (state, _owner, list) = setup(&pool).await;
    let (borrower_state, borrower_auth) = borrower(&pool).await;
    let actor = admin(&pool).await;
    publish_list(&pool, list).await;
    let _task = create(&borrower_state, &borrower_auth, list, "longterm").await;
    sqlx::query("UPDATE wordlists SET state='withdrawn' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let impact = admin_call(
        &state,
        actor,
        "GET",
        &format!("/api/v1/admin/wordlists/{list}/delete-impact"),
        Value::Null,
    )
    .await;
    assert_eq!(impact.0, 200, "{}", impact.1);
    assert_eq!(
        impact.1,
        json!({
            "active_run_count": 0,
            "active_task_count": 0,
            "affected_user_count": 0,
            "blocked_task_count": 1
        })
    );
}

#[sqlx::test]
async fn deletion_does_not_revive_a_run_from_an_older_public_generation(pool: PgPool) {
    use wordlists_support::{admin, admin_call};
    let (state, owner, list) = setup(&pool).await;
    let (borrower_state, borrower_auth) = borrower(&pool).await;
    let actor = admin(&pool).await;
    publish_list(&pool, list).await;
    let task = create(&borrower_state, &borrower_auth, list, "longterm").await;
    let run = start(&borrower_state, &borrower_auth, &task).await;

    let withdrawn = call(
        &state,
        &owner,
        "POST",
        &format!("/api/v1/me/wordlists/{list}/withdraw"),
        json!({"expected_revision":1}),
    )
    .await;
    assert_eq!(withdrawn.0, 200, "{}", withdrawn.1);
    let run_path = format!("/api/v1/me/learning-runs/{}", run["id"].as_str().unwrap());
    let invalidated = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &run_path,
        Value::Null,
    )
    .await;
    assert_eq!(invalidated.1["state"], "invalidated", "{}", invalidated.1);

    // Republish the newer generation, then delete it. The old run must not revive.
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let detail = admin_call(
        &state,
        actor,
        "GET",
        &format!("/api/v1/admin/wordlists/{list}"),
        Value::Null,
    )
    .await;
    assert_eq!(detail.0, 200, "{}", detail.1);
    let revision = detail.1["wordlist"]["revision"].as_i64().unwrap();
    let deleted = admin_call(
        &state,
        actor,
        "POST",
        &format!("/api/v1/admin/wordlists/{list}/delete"),
        json!({"expected_revision":revision}),
    )
    .await;
    assert_eq!(deleted.0, 204, "{}", deleted.1);

    let still = call(
        &borrower_state,
        &borrower_auth,
        "GET",
        &run_path,
        Value::Null,
    )
    .await;
    assert_eq!(still.1["state"], "invalidated", "{}", still.1);
    let qs = questions(&borrower_state, &borrower_auth, &run).await;
    let answered = call(
        &borrower_state,
        &borrower_auth,
        "POST",
        &answer_path(&run),
        answer_body(&qs["items"][0], "definitely-wrong"),
    )
    .await;
    assert_eq!(answered.0, 409, "{}", answered.1);
}
