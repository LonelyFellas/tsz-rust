mod account_deletion_support;
mod learning_tasks_support;
mod wordlists_support;
use account_deletion_support::call;
use learning_tasks_support::*;
use serde_json::Value;
use sqlx::PgPool;
#[sqlx::test]
async fn source_update_archive_and_deleted_source_receipt(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "longterm").await;
    let start_input = start_body(None);
    let start_path = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    let run = call(&state, &auth, "POST", &start_path, start_input.clone())
        .await
        .1;
    let qs = questions(&state, &auth, &run).await;
    let path = answer_path(&run);
    let first = answer_body(&qs["items"][0], "apple");
    let a = call(&state, &auth, "POST", &path, first.clone()).await;
    assert_eq!(a.0, 200, "{}", a.1);
    assert_eq!(a.1["is_correct"], true);
    sqlx::query("UPDATE wordlists SET name='rename',revision=revision+1 WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let entry: uuid::Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    let admin = wordlists_support::admin(&pool).await;
    wordlists_support::publish(&pool, entry, admin, "new-answer", 3).await;
    let restored = call(
        &state,
        &auth,
        "GET",
        &format!("/api/v1/me/learning-runs/{}", run["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(restored.1["state"], "active");
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=$1")
        .bind(entry)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE lexicon.entries SET archived_at=NULL WHERE id=$1")
        .bind(entry)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &path,
            answer_body(&qs["items"][1], "wrong")
        )
        .await
        .0,
        409
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_completions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("DELETE FROM wordlists WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let replay = call(&state, &auth, "POST", &path, first).await;
    assert_eq!(replay.0, 200, "{}", replay.1);
    assert_eq!(replay.1["answer_id"], a.1["answer_id"]);
    assert!(replay.1["question"]["feedback"].is_null());
    assert!(replay.1["question"]["prompt"].is_null());
    let replay = call(&state, &auth, "POST", &start_path, start_input).await;
    assert_eq!(replay.0, 200);
    assert_eq!(replay.1["id"], run["id"]);
}
#[sqlx::test]
async fn longterm_successor_concurrency_and_final_answer_rollback(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "longterm").await;
    let run = start(&state, &auth, &task).await;
    let qs = questions(&state, &auth, &run).await;
    let path = answer_path(&run);
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &path,
            answer_body(&qs["items"][0], "wrong")
        )
        .await
        .0,
        200
    );
    sqlx::raw_sql("CREATE FUNCTION fail_learning_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test completion failure'; END $$; CREATE TRIGGER fail_learning_completion BEFORE INSERT ON learning_completions FOR EACH ROW EXECUTE FUNCTION fail_learning_completion();").execute(&pool).await.unwrap();
    let last = answer_body(&qs["items"][1], "apple");
    assert_eq!(
        call(&state, &auth, "POST", &path, last.clone()).await.0,
        500
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_answers")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    sqlx::query("DROP TRIGGER fail_learning_completion ON learning_completions")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(call(&state, &auth, "POST", &path, last).await.0, 200);
    let sp = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    assert_eq!(
        call(&state, &auth, "POST", &sp, start_body(None)).await.0,
        409
    );
    let body = start_body(run["id"].as_str());
    let other = start_body(run["id"].as_str());
    let (a, b) = tokio::join!(
        call(&state, &auth, "POST", &sp, body.clone()),
        call(&state, &auth, "POST", &sp, other)
    );
    assert!([a.0.as_u16(), b.0.as_u16()].contains(&200));
    assert!([a.0.as_u16(), b.0.as_u16()].contains(&409));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    let resumed = start(&state, &auth, &task).await;
    assert_ne!(resumed["id"], run["id"]);
}
#[sqlx::test]
async fn expiry_gate_and_old_keys_replay(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "daily").await;
    let input = start_body(None);
    let sp = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    let run = call(&state, &auth, "POST", &sp, input.clone()).await.1;
    let qs = questions(&state, &auth, &run).await;
    let first = answer_body(&qs["items"][0], "apple");
    let path = answer_path(&run);
    assert_eq!(
        call(&state, &auth, "POST", &path, first.clone()).await.0,
        200
    );
    sqlx::query("UPDATE learning_runs SET expires_at=clock_timestamp()-interval '1 second',business_day=business_day-1 WHERE id=$1").bind(uuid::Uuid::parse_str(run["id"].as_str().unwrap()).unwrap()).execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, &auth, "POST", &sp, input).await.1["id"],
        run["id"]
    );
    let replay = call(&state, &auth, "POST", &path, first).await;
    assert_eq!(replay.0, 200);
    assert_eq!(replay.1["run"]["state"], "expired");
    assert!(replay.1["question"]["feedback"].is_object());
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &path,
            answer_body(&qs["items"][1], "apple")
        )
        .await
        .0,
        409
    );
    let fresh = start(&state, &auth, &task).await;
    assert_ne!(fresh["id"], run["id"]);
    let req = account_deletion_support::apply(&state, &auth, "0").await;
    account_deletion_support::deadline(&pool, req.id, -1).await;
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
        401
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn observed_invalidation_survives_failed_reopen(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "longterm").await;
    let run = start(&state, &auth, &task).await;
    let entry: uuid::Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let path = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    assert_eq!(
        call(&state, &auth, "POST", &path, start_body(None)).await.0,
        409
    );
    sqlx::query("INSERT INTO wordlist_items(wordlist_id,entry_id,position) VALUES($1,$2,0)")
        .bind(list)
        .bind(entry)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &auth, "POST", &path, start_body(None)).await.0,
        409
    );
    let next = call(&state, &auth, "POST", &path, start_body(run["id"].as_str())).await;
    assert_eq!(next.0, 200, "{}", next.1);
    assert_ne!(next.1["id"], run["id"]);
}
#[sqlx::test]
async fn start_write_crossing_deadline_rolls_back(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "daily").await;
    sqlx::query("UPDATE learning_tasks SET ends_at=clock_timestamp()+interval '0.7 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION slow_learning_start() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(1); RETURN NEW; END $$; CREATE TRIGGER slow_learning_start BEFORE INSERT ON learning_run_start_requests FOR EACH ROW EXECUTE FUNCTION slow_learning_start();").execute(&pool).await.unwrap();
    let path = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    assert_eq!(
        call(&state, &auth, "POST", &path, start_body(None)).await.0,
        409
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn source_owner_deletion_crossing_start_write_rolls_back(pool: PgPool) {
    let (state, owner, list) = setup(&pool).await;
    let (_, learner, _) = setup(&pool).await;
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let task = create(&state, &learner, list, "longterm").await;
    let request = account_deletion_support::apply(&state, &owner, "0").await;
    account_deletion_support::deadline(&pool, request.id, 700).await;
    sqlx::raw_sql("CREATE FUNCTION slow_learning_start() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(1); RETURN NEW; END $$; CREATE TRIGGER slow_learning_start BEFORE INSERT ON learning_run_start_requests FOR EACH ROW EXECUTE FUNCTION slow_learning_start();").execute(&pool).await.unwrap();
    let path = format!(
        "/api/v1/me/learning-tasks/{}/runs",
        task["id"].as_str().unwrap()
    );
    assert_eq!(
        call(&state, &learner, "POST", &path, start_body(None))
            .await
            .0,
        409
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn unrelated_large_append_preserves_run_and_large_pages_do_not_overflow(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    let task = create(&state, &auth, list, "longterm").await;
    let run = start(&state, &auth, &task).await;
    let admin = wordlists_support::admin(&pool).await;
    let entries:Vec<uuid::Uuid>=sqlx::query_scalar("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,detection_snapshot,created_by_admin_id,updated_by_admin_id) SELECT gen_random_uuid(),3,'en','word','{}',$1,$1 FROM generate_series(1,1000) RETURNING id").bind(admin).fetch_all(&pool).await.unwrap();
    sqlx::query("INSERT INTO wordlist_items(wordlist_id,entry_id,position) SELECT $1,x.id,x.n::integer FROM unnest($2::uuid[]) WITH ORDINALITY AS x(id,n)").bind(list).bind(entries).execute(&pool).await.unwrap();
    let recovered = start(&state, &auth, &task).await;
    assert_eq!(recovered["id"], run["id"]);
    assert_eq!(recovered["state"], "active");
    let qs = questions(&state, &auth, &run).await;
    for q in qs["items"].as_array().unwrap() {
        assert_eq!(
            call(
                &state,
                &auth,
                "POST",
                &answer_path(&run),
                answer_body(q, "wrong")
            )
            .await
            .0,
            200
        );
    }
    for path in [
        "/api/v1/me/learning-tasks".to_owned(),
        format!(
            "/api/v1/me/learning-tasks/{}/runs",
            task["id"].as_str().unwrap()
        ),
        format!(
            "/api/v1/me/learning-runs/{}/questions",
            run["id"].as_str().unwrap()
        ),
    ] {
        let r = call(
            &state,
            &auth,
            "GET",
            &format!("{path}?page=4294967295&page_size=50"),
            Value::Null,
        )
        .await;
        assert_eq!(r.0, 200, "{}", r.1);
        assert_eq!(r.1["items"], serde_json::json!([]));
    }
}
