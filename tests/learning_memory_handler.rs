mod account_deletion_support;
mod learning_tasks_support;
mod wordlists_support;
use account_deletion_support::call;
use learning_tasks_support::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn enroll(pool: &PgPool, user: Uuid) {
    sqlx::query("INSERT INTO learning_memory_enrollments(user_id,enabled) VALUES($1,true)")
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test]
async fn default_disabled_then_cross_wordlist_and_concurrent_replay(pool: PgPool) {
    let (mut state, auth, list) = setup(&pool).await;
    state.learning_memory_enabled = true;
    let task = create(&state, &auth, list, "longterm").await;
    let run = start(&state, &auth, &task).await;
    let qs = questions(&state, &auth, &run).await;
    let path = answer_path(&run);
    let first = answer_body(&qs["items"][0], "apple");
    assert_eq!(
        call(&state, &auth, "POST", &path, first.clone()).await.0,
        200
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    enroll(&pool, auth.subject).await;
    assert_eq!(call(&state, &auth, "POST", &path, first).await.0, 200);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_memory_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let entry: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    let other = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        wordlists_support::create_body(&[entry]),
    )
    .await;
    assert_eq!(other.0, 200);
    let other = Uuid::parse_str(other.1["id"].as_str().unwrap()).unwrap();
    let a = create(&state, &auth, list, "longterm").await;
    let b = create(&state, &auth, other, "longterm").await;
    let ra = start(&state, &auth, &a).await;
    let rb = start(&state, &auth, &b).await;
    let qa = questions(&state, &auth, &ra).await;
    let qb = questions(&state, &auth, &rb).await;
    let body = answer_body(&qa["items"][0], "apple");
    let pa = answer_path(&ra);
    let (x, y) = tokio::join!(
        call(&state, &auth, "POST", &pa, body.clone()),
        call(&state, &auth, "POST", &pa, body.clone())
    );
    assert_eq!(x.0, 200, "{}", x.1);
    assert_eq!(y.0, 200, "{}", y.1);
    assert_eq!(x.1["answer_id"], y.1["answer_id"]);
    let before: (Uuid, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT id,due_at FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &answer_path(&rb),
            answer_body(&qb["items"][0], "apple")
        )
        .await
        .0,
        200
    );
    let after: (Uuid, chrono::DateTime<chrono::Utc>, i16, i64) =
        sqlx::query_as("SELECT id,due_at,interval_step,revision FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(after, (before.0, before.1, 0, 2));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_memory_sources")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_memory_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &pa,
            answer_body(&qa["items"][0], "wrong")
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
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    for table in [
        "learning_memories",
        "learning_memory_sources",
        "learning_memory_events",
        "learning_memory_enrollments",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM {table}"
            )))
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }
}

#[sqlx::test]
async fn completion_and_memory_roll_back_together(pool: PgPool) {
    let (mut state, auth, list) = setup(&pool).await;
    state.learning_memory_enabled = true;
    enroll(&pool, auth.subject).await;
    let task = create(&state, &auth, list, "daily").await;
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
    sqlx::raw_sql("CREATE FUNCTION fail_memory_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture memory failure'; END $$; CREATE TRIGGER fail_memory_event BEFORE INSERT ON learning_memory_events FOR EACH ROW EXECUTE FUNCTION fail_memory_event();").execute(&pool).await.unwrap();
    let last = answer_body(&qs["items"][1], "wrong");
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
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_completions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    sqlx::raw_sql("DROP TRIGGER fail_memory_event ON learning_memory_events; DROP FUNCTION fail_memory_event();").execute(&pool).await.unwrap();
    let completed = call(&state, &auth, "POST", &path, last).await;
    assert_eq!(completed.0, 200, "{}", completed.1);
    assert_eq!(completed.1["run"]["state"], "completed");
    assert_eq!(completed.1["run"]["correct_count"], 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM learning_memories WHERE stage='relearning'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
    let fields = completed
        .1
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        fields,
        [
            "answer_id",
            "question_id",
            "is_correct",
            "accepted_at",
            "run",
            "question"
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
}

#[sqlx::test]
async fn content_epoch_protects_new_memory_from_old_questions(pool: PgPool) {
    let (mut state, auth, list) = setup(&pool).await;
    state.learning_memory_enabled = true;
    enroll(&pool, auth.subject).await;
    let original = create(&state, &auth, list, "longterm").await;
    let old = start(&state, &auth, &original).await;
    let oq = questions(&state, &auth, &old).await;
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &answer_path(&old),
            answer_body(&oq["items"][0], "apple")
        )
        .await
        .0,
        200
    );
    let another = create(&state, &auth, list, "longterm").await;
    let stale = start(&state, &auth, &another).await;
    let sq = questions(&state, &auth, &stale).await;
    let entry: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut snapshot:Value=sqlx::query_scalar("SELECT p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(entry).fetch_one(&pool).await.unwrap();
    snapshot["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] =
        json!("新的中文释义");
    let publication = Uuid::now_v7();
    let admin = wordlists_support::admin(&pool).await;
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) VALUES($1,$2,3,3,3,$3,$4,$5)").bind(publication).bind(entry).bind(snapshot).bind(publication.as_bytes().to_vec()).bind(admin).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=$2 WHERE id=$1")
        .bind(entry)
        .bind(publication)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &answer_path(&stale),
            answer_body(&sq["items"][0], "apple")
        )
        .await
        .0,
        200
    );
    let memory: (i64, String) = sqlx::query_as("SELECT memory_epoch,stage FROM learning_memories")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(memory, (2, "unverified".into()));
    let fresh_task = create(&state, &auth, list, "longterm").await;
    let fresh = start(&state, &auth, &fresh_task).await;
    let fq = questions(&state, &auth, &fresh).await;
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &answer_path(&fresh),
            answer_body(&fq["items"][0], "apple")
        )
        .await
        .0,
        200
    );
    let memory: (i64, String, i16) =
        sqlx::query_as("SELECT memory_epoch,stage,interval_step FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(memory, (2, "learning".into(), 0));
}

#[sqlx::test]
async fn migration_empty_roundtrip_and_populated_rollback_guard(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../migrations/20261024030000_learning_memory_retention.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261024020000_learning_review.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261024010000_learning_memory.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261024010000_learning_memory.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261024020000_learning_review.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261024030000_learning_memory_retention.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let (_, auth, _) = setup(&pool).await;
    enroll(&pool, auth.subject).await;
    let mut tx = pool.begin().await.unwrap();
    let error = sqlx::raw_sql(include_str!(
        "../migrations/20261024010000_learning_memory.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("personal learning memory data exists")
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_memory_enrollments")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

async fn ready_memory(
    pool: &PgPool,
    wrong: bool,
) -> (
    tsz_rust::state::AppState,
    tsz_rust::auth::extract::AuthUser,
    Uuid,
) {
    let (mut state, auth, list) = setup(pool).await;
    state.learning_memory_enabled = true;
    let enabled = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/memory/enrollment",
        json!({"enabled":true}),
    )
    .await;
    assert_eq!(enabled.0, 200, "{}", enabled.1);
    let task = create(&state, &auth, list, "longterm").await;
    let run = start(&state, &auth, &task).await;
    let qs = questions(&state, &auth, &run).await;
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &answer_path(&run),
            answer_body(&qs["items"][0], if wrong { "wrong" } else { "apple" })
        )
        .await
        .0,
        200
    );
    sqlx::query("UPDATE learning_memories SET due_at=clock_timestamp()-interval '1 minute',last_feedback_at=clock_timestamp()-interval '11 minutes' WHERE user_id=$1").bind(auth.subject).execute(pool).await.unwrap();
    (state, auth, list)
}
fn review_body(mode: &str) -> Value {
    json!({"idempotency_key":Uuid::now_v7(),"mode":mode,"limit":20})
}
#[sqlx::test]
async fn review_due_answer_is_idempotent_and_never_completes_or_rewards(pool: PgPool) {
    let (state, auth, _) = ready_memory(&pool, false).await;
    let overview = call(
        &state,
        &auth,
        "GET",
        "/api/v1/me/memory/overview",
        Value::Null,
    )
    .await;
    assert_eq!(overview.0, 200, "{}", overview.1);
    assert_eq!(overview.1["due_count"], 1);
    let body = review_body("due");
    let session = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/review-sessions",
        body.clone(),
    )
    .await;
    assert_eq!(session.0, 200, "{}", session.1);
    let replay = call(&state, &auth, "POST", "/api/v1/me/review-sessions", body).await;
    assert_eq!(replay.1["id"], session.1["id"]);
    assert!(session.1["items"][0]["feedback"].is_null());
    assert!(session.1["items"][0]["revealed_answers"].is_null());
    let path = format!(
        "/api/v1/me/review-sessions/{}/answers",
        session.1["id"].as_str().unwrap()
    );
    let answer = json!({"idempotency_key":Uuid::now_v7(),"item_id":session.1["items"][0]["id"],"answer":"apple"});
    let (a, b) = tokio::join!(
        call(&state, &auth, "POST", &path, answer.clone()),
        call(&state, &auth, "POST", &path, answer)
    );
    assert_eq!(a.0, 200, "{}", a.1);
    assert_eq!(b.0, 200, "{}", b.1);
    assert_eq!(a.1["answer_id"], b.1["answer_id"]);
    assert_eq!(a.1["memory_effect"], "review_passed");
    assert_eq!(a.1["session"]["state"], "completed");
    assert_eq!(
        sqlx::query_scalar::<_, i16>("SELECT interval_step FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_completions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_reward_settlements")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_review_attempts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
#[sqlx::test]
async fn retest_budget_shared_between_sessions_and_hint_does_not_promote(pool: PgPool) {
    let (state, auth, _) = ready_memory(&pool, true).await;
    let a = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/review-sessions",
        review_body("retest"),
    )
    .await;
    assert_eq!(a.0, 200, "{}", a.1);
    let b = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/review-sessions",
        review_body("retest"),
    )
    .await;
    assert_eq!(a.1["id"], b.1["id"]);
    assert_eq!(
        sqlx::query_scalar::<_, i16>("SELECT retest_issued FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let id = a.1["id"].as_str().unwrap();
    let item = a.1["items"][0]["id"].as_str().unwrap();
    let reveal = call(
        &state,
        &auth,
        "POST",
        &format!("/api/v1/me/review-sessions/{id}/items/{item}/reveal"),
        json!({}),
    )
    .await;
    assert_eq!(reveal.0, 200, "{}", reveal.1);
    assert!(reveal.1["items"][0]["revealed_answers"].is_array());
    let answer = call(
        &state,
        &auth,
        "POST",
        &format!("/api/v1/me/review-sessions/{id}/answers"),
        json!({"idempotency_key":Uuid::now_v7(),"item_id":item,"answer":"apple"}),
    )
    .await;
    assert_eq!(answer.0, 200, "{}", answer.1);
    assert_eq!(answer.1["memory_effect"], "practice_assisted");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT stage FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "relearning"
    );
    sqlx::query(
        "UPDATE learning_memories SET last_feedback_at=clock_timestamp()-interval '11 minutes'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let second = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/review-sessions",
        review_body("retest"),
    )
    .await;
    assert_eq!(second.0, 200, "{}", second.1);
    let result=call(&state,&auth,"POST",&format!("/api/v1/me/review-sessions/{}/answers",second.1["id"].as_str().unwrap()),json!({"idempotency_key":Uuid::now_v7(),"item_id":second.1["items"][0]["id"],"answer":"wrong"})).await;
    assert_eq!(result.0, 200, "{}", result.1);
    sqlx::query(
        "UPDATE learning_memories SET last_feedback_at=clock_timestamp()-interval '11 minutes'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/review-sessions",
            review_body("retest")
        )
        .await
        .0,
        409
    );
    assert_eq!(
        sqlx::query_scalar::<_, i16>("SELECT retest_issued FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}
#[sqlx::test]
async fn revoked_review_hides_snapshots_and_rejects_answers(pool: PgPool) {
    let (state, auth, list) = ready_memory(&pool, false).await;
    let s = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/review-sessions",
        review_body("due"),
    )
    .await;
    assert_eq!(s.0, 200, "{}", s.1);
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id IN(SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1)").bind(list).execute(&pool).await.unwrap();
    let path = format!("/api/v1/me/review-sessions/{}", s.1["id"].as_str().unwrap());
    let hidden = call(&state, &auth, "GET", &path, Value::Null).await;
    assert_eq!(hidden.0, 200);
    assert!(hidden.1["items"][0]["prompt"].is_null());
    assert_eq!(call(&state,&auth,"POST",&format!("{path}/answers"),json!({"idempotency_key":Uuid::now_v7(),"item_id":s.1["items"][0]["id"],"answer":"apple"})).await.0,409);
    let page = call(&state, &auth, "GET", "/api/v1/me/memory/items", Value::Null).await;
    assert_eq!(page.0, 200, "{}", page.1);
    assert!(page.1["items"][0]["label"].is_null());
    let (_, other) = account_deletion_support::setup_bound(&pool).await;
    assert_eq!(call(&state, &other, "GET", &path, Value::Null).await.0, 409);
}

#[sqlx::test]
async fn normal_review_stale_evidence_and_paused_memory_are_not_overwritten(pool: PgPool) {
    let (state, auth, list) = ready_memory(&pool, false).await;
    let s = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/review-sessions",
        review_body("due"),
    )
    .await;
    assert_eq!(s.0, 200, "{}", s.1);
    let task = create(&state, &auth, list, "longterm").await;
    let run = start(&state, &auth, &task).await;
    let qs = questions(&state, &auth, &run).await;
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &answer_path(&run),
            answer_body(&qs["items"][0], "apple")
        )
        .await
        .0,
        200
    );
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM learning_memories")
        .fetch_one(&pool)
        .await
        .unwrap();
    let old = call(
        &state,
        &auth,
        "POST",
        &format!(
            "/api/v1/me/review-sessions/{}/answers",
            s.1["id"].as_str().unwrap()
        ),
        json!({"idempotency_key":Uuid::now_v7(),"item_id":s.1["items"][0]["id"],"answer":"wrong"}),
    )
    .await;
    assert_eq!(old.0, 200, "{}", old.1);
    assert_eq!(old.1["memory_effect"], "practice_only_stale");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap(),
        revision
    );
    let page = call(&state, &auth, "GET", "/api/v1/me/memory/items", Value::Null).await;
    let m = &page.1["items"][0];
    let pause = format!("/api/v1/me/memory/items/{}", m["id"].as_str().unwrap());
    assert_eq!(
        call(
            &state,
            &auth,
            "PATCH",
            &pause,
            json!({"paused":true,"expected_revision":m["revision"]})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "PATCH",
            &pause,
            json!({"paused":false,"expected_revision":m["revision"]})
        )
        .await
        .0,
        409
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            "/api/v1/me/memory/overview",
            Value::Null
        )
        .await
        .1["paused_count"],
        1
    );
}

#[sqlx::test]
async fn retest_crosses_four_am_by_expiring_not_spending_the_new_day(pool: PgPool) {
    let (mut state, auth, _) = ready_memory(&pool, true).await;
    sqlx::raw_sql("CREATE SCHEMA memory_clock; CREATE TABLE memory_clock.instant(at timestamptz); INSERT INTO memory_clock.instant VALUES('2030-01-01T19:59:00Z'); CREATE FUNCTION memory_clock.clock_timestamp() RETURNS timestamptz LANGUAGE sql VOLATILE AS 'SELECT at FROM memory_clock.instant';").execute(&pool).await.unwrap();
    let timed = sqlx::postgres::PgPoolOptions::new()
        .max_connections(3)
        .after_connect(|c, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO memory_clock,public,pg_catalog")
                    .execute(c)
                    .await?;
                Ok(())
            })
        })
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    state.pool = timed.clone();
    let s = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/review-sessions",
        review_body("retest"),
    )
    .await;
    assert_eq!(s.0, 200, "{}", s.1);
    assert_eq!(s.1["items"][0]["expires_at"], "2030-01-01T20:00:00Z");
    sqlx::query("UPDATE memory_clock.instant SET at='2030-01-01T20:01:00Z'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(call(&state,&auth,"POST",&format!("/api/v1/me/review-sessions/{}/answers",s.1["id"].as_str().unwrap()),json!({"idempotency_key":Uuid::now_v7(),"item_id":s.1["items"][0]["id"],"answer":"apple"})).await.0,409);
    let next = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/review-sessions",
        review_body("retest"),
    )
    .await;
    assert_eq!(next.0, 200, "{}", next.1);
    assert_ne!(next.1["id"], s.1["id"]);
    assert_eq!(
        sqlx::query_scalar::<_, i16>("SELECT retest_issued FROM learning_memories")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    timed.close().await;
}

#[sqlx::test]
async fn feature_gate_preserves_legacy_answers_without_memory_writes(pool: PgPool) {
    let (mut state, auth, list) = ready_memory(&pool, false).await;
    state.learning_memory_enabled = false;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM learning_memory_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    let task = create(&state, &auth, list, "longterm").await;
    let run = start(&state, &auth, &task).await;
    let qs = questions(&state, &auth, &run).await;
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &answer_path(&run),
            answer_body(&qs["items"][0], "wrong")
        )
        .await
        .0,
        200
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_memory_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/review-sessions",
            review_body("due")
        )
        .await
        .0,
        404
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            "/api/v1/me/memory/overview",
            Value::Null
        )
        .await
        .1["available"],
        false
    );
}
