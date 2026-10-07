mod account_deletion_support;
mod learning_tasks_support;
mod wordlists_support;
use account_deletion_support::call;
use learning_tasks_support::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use tsz_rust::{auth::extract::AuthUser, state::AppState};
use uuid::Uuid;
const REWARDS: &str = "/api/v1/me/coins/learning-rewards";

// Isolated fixture only: emulate a policy published before today's 04:00. The
// production schedule trigger is restored before any API call. No runtime bypass.
async fn policy(pool: &PgPool, enabled: bool, minimum: i32) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "ALTER TABLE learning_reward_policies DISABLE TRIGGER learning_reward_policy_schedule",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("INSERT INTO learning_reward_policies(id,rule_version,effective_business_day,enabled,daily_amount,minimum_units) VALUES($1,'fixture-v1',((clock_timestamp() AT TIME ZONE 'Asia/Shanghai')-interval '4 hours')::date,$2,13,$3)").bind(Uuid::now_v7()).bind(enabled).bind(minimum).execute(&mut *tx).await.unwrap();
    sqlx::query(
        "ALTER TABLE learning_reward_policies ENABLE TRIGGER learning_reward_policy_schedule",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
async fn complete(state: &AppState, auth: &AuthUser, list: Uuid, kind: &str) -> (Value, Value) {
    let task = create(state, auth, list, kind).await;
    let run = start(state, auth, &task).await;
    let qs = questions(state, auth, &run).await;
    let mut last = Value::Null;
    for q in qs["items"].as_array().unwrap() {
        last = answer_body(q, "wrong");
        let r = call(state, auth, "POST", &answer_path(&run), last.clone()).await;
        assert_eq!(r.0, 200, "{}", r.1);
    }
    (run, last)
}
async fn reward(state: &AppState, auth: &AuthUser) -> Value {
    let r = call(state, auth, "GET", REWARDS, Value::Null).await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
async fn issuance(pool: &PgPool) -> (i64, i64) {
    sqlx::query_as("SELECT count(*),coalesce(sum(e.delta),0)::bigint FROM coin_operations o JOIN coin_entries e ON e.operation_id=o.id WHERE o.source_type='learning_reward'").fetch_one(pool).await.unwrap()
}
#[sqlx::test]
async fn distinct_completed_units_and_concurrent_day_limit(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    policy(&pool, true, 3).await;
    complete(&state, &auth, list, "daily").await;
    assert_eq!(reward(&state, &auth).await["qualifying_units"], 2);
    complete(&state, &auth, list, "daily").await;
    assert_eq!(reward(&state, &auth).await["qualifying_units"], 2);
    assert_eq!(issuance(&pool).await, (0, 0));
    let entry = wordlists_support::full_entry(&pool, "banana").await;
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
    let (a, b) = tokio::join!(
        complete(&state, &auth, other, "daily"),
        complete(&state, &auth, other, "daily")
    );
    let summary = reward(&state, &auth).await;
    assert_eq!(summary["status"], "awarded");
    assert_eq!(summary["awarded_amount"], "13");
    assert_eq!(summary["qualifying_units"], 3);
    assert_eq!(issuance(&pool).await, (1, 13));
    for (run, body) in [a, b] {
        let r = call(&state, &auth, "POST", &answer_path(&run), body).await;
        assert_eq!(r.0, 200);
        assert_eq!(r.1["run"]["state"], "completed");
        assert_eq!(r.1["run"]["correct_count"], 0);
    }
    assert_eq!(issuance(&pool).await, (1, 13));
    // Retained minimal audit must agree with the real ledger and survive physical deletion.
    let coherent:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM learning_reward_settlements s JOIN coin_operations o ON o.id=s.operation_id JOIN coin_entries e ON e.operation_id=o.id JOIN coin_wallets w ON w.id=e.wallet_id WHERE s.user_id=$1 AND w.owner_id=s.user_id AND e.delta=s.awarded_amount AND o.source_id=s.user_id::text||':'||s.business_day::text)").bind(auth.subject).fetch_one(&pool).await.unwrap();
    assert!(coherent);
    let deletion = account_deletion_support::apply(&state, &auth, "13").await;
    account_deletion_support::deadline(&pool, deletion.id, -1).await;
    assert!(
        tsz_rust::account_deletion::service::complete(&pool, auth.subject, deletion.id)
            .await
            .unwrap()
    );
    let closed:(String,i64,bool)=sqlx::query_as("SELECT w.status,w.balance,d.waive_balance FROM coin_wallets w JOIN account_deletion_requests d ON d.user_id=w.owner_id WHERE d.id=$1").bind(deletion.id).fetch_one(&pool).await.unwrap();
    assert_eq!(closed, ("closed".into(), 0, true));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM coin_operations WHERE kind='account_closure_forfeit'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        tsz_rust::coins::repository::reconcile(&pool)
            .await
            .unwrap()
            .wallet_mismatches,
        0
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
        1
    );
    assert_eq!(issuance(&pool).await, (1, 13));
}
#[sqlx::test]
async fn disabled_longterm_and_paused_completion_are_not_credited(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    assert_eq!(reward(&state, &auth).await["status"], "reward_disabled");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_reward_settlements")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    complete(&state, &auth, list, "longterm").await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_reward_settlements")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    complete(&state, &auth, list, "daily").await;
    assert_eq!(reward(&state, &auth).await["status"], "reward_disabled");
    policy(&pool, true, 2).await; // Even a fixture changing availability cannot replay a terminal day.
    complete(&state, &auth, list, "daily").await;
    assert_eq!(issuance(&pool).await, (0, 0));
    let (s2, a2, l2) = setup(&pool).await;
    let deletion = account_deletion_support::apply(&s2, &a2, "0").await;
    complete(&s2, &a2, l2, "daily").await;
    assert_eq!(reward(&s2, &a2).await["status"], "wallet_unavailable");
    let cancelled = call(
        &s2,
        &a2,
        "POST",
        &format!("/api/v1/me/account-deletion/{}/cancel", deletion.id),
        json!({}),
    )
    .await;
    assert_eq!(cancelled.0, 200, "{}", cancelled.1);
    complete(&s2, &a2, l2, "daily").await;
    assert_eq!(reward(&s2, &a2).await["status"], "wallet_unavailable");
    assert_eq!(issuance(&pool).await, (0, 0));
}
#[sqlx::test]
async fn audit_failure_and_deadline_after_reward_roll_back_everything(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    policy(&pool, true, 2).await;
    let task = create(&state, &auth, list, "daily").await;
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
    sqlx::raw_sql("CREATE FUNCTION fail_reward() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected audit failure'; END $$; CREATE TRIGGER fail_reward BEFORE INSERT ON learning_reward_settlements FOR EACH ROW EXECUTE FUNCTION fail_reward();").execute(&pool).await.unwrap();
    let body = answer_body(&qs["items"][1], "wrong");
    assert_eq!(
        call(&state, &auth, "POST", &answer_path(&run), body.clone())
            .await
            .0,
        500
    );
    assert_eq!(issuance(&pool).await, (0, 0));
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
    sqlx::raw_sql(
        "DROP TRIGGER fail_reward ON learning_reward_settlements; DROP FUNCTION fail_reward();",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(72120400)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE learning_runs SET expires_at=clock_timestamp()+interval '1 second' WHERE id=$1",
    )
    .bind(Uuid::parse_str(run["id"].as_str().unwrap()).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let path = answer_path(&run);
    let pending = call(&state, &auth, "POST", &path, body.clone());
    let release = async {
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        blocker.commit().await.unwrap();
    };
    let (r, ()) = tokio::join!(pending, release);
    assert_eq!(r.0, 409, "{}", r.1);
    assert_eq!(issuance(&pool).await, (0, 0));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_completions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query(
        "UPDATE learning_runs SET expires_at=clock_timestamp()+interval '1 hour' WHERE id=$1",
    )
    .bind(Uuid::parse_str(run["id"].as_str().unwrap()).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        call(&state, &auth, "POST", &answer_path(&run), body)
            .await
            .0,
        200
    );
    assert_eq!(issuance(&pool).await, (1, 13));
}
#[sqlx::test]
async fn query_is_read_only_scoped_and_validates_dates(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    policy(&pool, true, 2).await;
    complete(&state, &auth, list, "daily").await;
    let (other, b, _) = setup(&pool).await;
    assert_eq!(reward(&other, &b).await["status"], "in_progress");
    for q in [
        "business_day=2999-01-01",
        "business_day=2026-02-30",
        "user_id=someone",
    ] {
        assert_eq!(
            call(&state, &auth, "GET", &format!("{REWARDS}?{q}"), Value::Null)
                .await
                .0,
            400
        );
    }
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!("{REWARDS}?business_day=2020-01-01"),
            Value::Null
        )
        .await
        .1["status"],
        "reward_disabled"
    );
    sqlx::query("DELETE FROM user_roles WHERE user_id=$1")
        .bind(b.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(call(&other, &b, "GET", REWARDS, Value::Null).await.0, 403);
    assert_eq!(issuance(&pool).await, (1, 13));
}

#[sqlx::test]
async fn duplicate_short_task_query_budget(pool: PgPool) {
    let (state, auth, list) = setup(&pool).await;
    policy(&pool, true, 10).await;
    let task = create(&state, &auth, list, "daily").await;
    let run = start(&state, &auth, &task).await;
    let run_id = Uuid::parse_str(run["id"].as_str().unwrap()).unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql("CREATE TEMP TABLE reward_load AS SELECT n,gen_random_uuid() task_id,gen_random_uuid() run_id,gen_random_uuid() question_id FROM generate_series(1,10000) n;").execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO learning_tasks(id,user_id,name,task_type,wordlist_ids,daily_question_count,create_key,create_hash) SELECT x.task_id,t.user_id,t.name,t.task_type,t.wordlist_ids,1,gen_random_uuid(),t.create_hash FROM reward_load x CROSS JOIN learning_tasks t WHERE t.id=$1").bind(Uuid::parse_str(task["id"].as_str().unwrap()).unwrap()).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO learning_runs(id,task_id,user_id,task_revision,task_type,business_day,window_start,window_end,expires_at,settings_snapshot,seed,target_count,state,generation_version,grading_version) SELECT x.run_id,x.task_id,r.user_id,1,'daily',r.business_day,r.window_start,r.window_end,r.expires_at,r.settings_snapshot,gen_random_uuid(),1,'completed',r.generation_version,r.grading_version FROM reward_load x CROSS JOIN learning_runs r WHERE r.id=$1").bind(run_id).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO learning_questions(id,run_id,position,source_wordlist_id,source_revision,source_membership_id,source_public_generation,entry_id,entry_archive_generation,publication_id,pos_id,sense_id,definition_id,form_ids,unit_key,prompt_snapshot,answer_snapshot,fingerprint) SELECT x.question_id,x.run_id,0,q.source_wordlist_id,q.source_revision,q.source_membership_id,q.source_public_generation,q.entry_id,q.entry_archive_generation,q.publication_id,q.pos_id,q.sense_id,q.definition_id,q.form_ids,q.unit_key,q.prompt_snapshot,q.answer_snapshot,q.fingerprint FROM reward_load x CROSS JOIN learning_questions q WHERE q.run_id=$1 AND q.position=0").bind(run_id).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql("INSERT INTO learning_answers(id,run_id,question_id,request_key,request_hash,submitted_answer,normalized_answer,is_correct,accepted_at,grading_version) SELECT gen_random_uuid(),x.run_id,x.question_id,gen_random_uuid(),'hash','wrong','wrong',false,clock_timestamp(),r.grading_version FROM reward_load x JOIN learning_runs r ON r.id=x.run_id;
    INSERT INTO learning_completions(id,run_id,task_id,user_id,task_type,business_day,task_revision,target_count,answered_count,correct_count,completed_at,generation_version,grading_version) SELECT gen_random_uuid(),r.id,r.task_id,r.user_id,'daily',r.business_day,1,1,1,0,clock_timestamp(),r.generation_version,r.grading_version FROM reward_load x JOIN learning_runs r ON r.id=x.run_id;
    ANALYZE learning_completions; ANALYZE learning_questions; SET LOCAL statement_timeout='1000ms';").execute(&mut *tx).await.unwrap();
    let query = "SELECT count(*)::integer FROM (SELECT DISTINCT q.unit_key FROM learning_completions c JOIN learning_questions q ON q.run_id=c.run_id WHERE c.user_id=$1 AND c.business_day=$2 AND c.task_type='daily' LIMIT $3) units";
    let day = run["business_day"]
        .as_str()
        .unwrap()
        .parse::<chrono::NaiveDate>()
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i32>(query)
            .bind(auth.subject)
            .bind(day)
            .bind(10_i32)
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
        1
    );
    let plan: Vec<String> = sqlx::query_scalar("EXPLAIN (ANALYZE,BUFFERS) SELECT count(*)::integer FROM (SELECT DISTINCT q.unit_key FROM learning_completions c JOIN learning_questions q ON q.run_id=c.run_id WHERE c.user_id=$1 AND c.business_day=$2 AND c.task_type='daily' LIMIT $3) units")
        .bind(auth.subject)
        .bind(day)
        .bind(10_i32)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    println!("10,000 duplicate completed units: {}", plan.join("\n"));
    // Fixture identities, not production semantic mutation: exercise bounded distinct output.
    sqlx::raw_sql("UPDATE learning_questions q SET unit_key='fixture-unit-'||(x.n%20)::text FROM reward_load x WHERE q.id=x.question_id").execute(&mut *tx).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i32>(query)
            .bind(auth.subject)
            .bind(day)
            .bind(10_i32)
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
        10
    );
    tx.rollback().await.unwrap();
}

#[sqlx::test]
async fn policy_publication_and_replay_across_four_am(pool: PgPool) {
    // The clock exists only in this disposable test database/search_path. Production
    // code has no injectable clock, header or environment override.
    sqlx::raw_sql("CREATE SCHEMA reward_clock; CREATE TABLE reward_clock.instant(at timestamptz NOT NULL); INSERT INTO reward_clock.instant VALUES('2026-10-07T06:00:00Z'); CREATE FUNCTION reward_clock.clock_timestamp() RETURNS timestamptz LANGUAGE sql VOLATILE AS 'SELECT at FROM reward_clock.instant';").execute(&pool).await.unwrap();
    let timed = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .after_connect(|c, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO reward_clock,public,pg_catalog")
                    .execute(c)
                    .await?;
                Ok(())
            })
        })
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    for (date, version, amount) in [("2026-10-08", "v1", 13_i64), ("2026-10-09", "v2", 21_i64)] {
        sqlx::query("INSERT INTO learning_reward_policies(id,rule_version,effective_business_day,enabled,daily_amount,minimum_units) VALUES($1,$2,$3,true,$4,2)").bind(Uuid::now_v7()).bind(version).bind(date.parse::<chrono::NaiveDate>().unwrap()).bind(amount).execute(&timed).await.unwrap();
    }
    sqlx::query("UPDATE reward_clock.instant SET at='2026-10-08T19:59:59Z'")
        .execute(&pool)
        .await
        .unwrap();
    let (state, auth, list) = setup(&timed).await;
    let (completed, body) = complete(&state, &auth, list, "daily").await;
    assert_eq!(reward(&state, &auth).await["awarded_amount"], "13");
    let task = create(&state, &auth, list, "daily").await;
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
    // A separate user avoids the existing day terminal, so the last answer waits on policy.
    let (other, b, list_b) = setup(&timed).await;
    let task_b = create(&other, &b, list_b, "daily").await;
    let run_b = start(&other, &b, &task_b).await;
    let qb = questions(&other, &b, &run_b).await;
    assert_eq!(
        call(
            &other,
            &b,
            "POST",
            &answer_path(&run_b),
            answer_body(&qb["items"][0], "wrong")
        )
        .await
        .0,
        200
    );
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(72120400)")
        .execute(&mut *lock)
        .await
        .unwrap();
    let path = answer_path(&run_b);
    let pending = call(
        &other,
        &b,
        "POST",
        &path,
        answer_body(&qb["items"][1], "wrong"),
    );
    let writer=sqlx::query("INSERT INTO learning_reward_policies(id,rule_version,effective_business_day,enabled) VALUES($1,'late','2026-10-09',false)").bind(Uuid::now_v7()).execute(&timed);
    let release = async {
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            loop {
                let n:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory'").fetch_one(&pool).await.unwrap();
                if n>=2 {break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        sqlx::query("UPDATE reward_clock.instant SET at='2026-10-08T20:00:00Z'")
            .execute(&pool)
            .await
            .unwrap();
        lock.commit().await.unwrap();
    };
    let (answer, publication, ()) = tokio::join!(pending, writer, release);
    assert_eq!(answer.0, 409, "{}", answer.1);
    assert!(
        publication
            .unwrap_err()
            .to_string()
            .contains("future business day")
    );
    let replay = call(&state, &auth, "POST", &answer_path(&completed), body).await;
    assert_eq!(replay.0, 200);
    assert!(replay.1["run"]["completion_id"].is_string());
    let historical = call(
        &state,
        &auth,
        "GET",
        &format!("{REWARDS}?business_day=2026-10-08"),
        Value::Null,
    )
    .await;
    assert_eq!(historical.1["awarded_amount"], "13");
    assert_eq!(historical.1["rule_version"], "v1");
    assert_eq!(reward(&state, &auth).await["daily_amount"], "21");
    assert_eq!(reward(&other, &b).await["rule_version"], "v2");
    complete(&state, &auth, list, "daily").await;
    assert_eq!(reward(&state, &auth).await["awarded_amount"], "21");
    assert_eq!(issuance(&pool).await, (2, 34));
    // Exercise both the ledger's wallet wait and the initial account wait across 04:00.
    sqlx::query("INSERT INTO coin_wallets(id,owner_type,owner_id) VALUES($1,'user',$2)")
        .bind(Uuid::now_v7())
        .bind(b.subject)
        .execute(&pool)
        .await
        .unwrap();
    for (before, after, wallet_wait) in [
        ("2026-10-09T19:59:59Z", "2026-10-09T20:00:00Z", true),
        ("2026-10-10T19:59:59Z", "2026-10-10T20:00:00Z", false),
    ] {
        sqlx::query("UPDATE reward_clock.instant SET at=$1")
            .bind(before.parse::<chrono::DateTime<chrono::Utc>>().unwrap())
            .execute(&pool)
            .await
            .unwrap();
        let task = create(&other, &b, list_b, "daily").await;
        let run = start(&other, &b, &task).await;
        let questions = questions(&other, &b, &run).await;
        let path = answer_path(&run);
        assert_eq!(
            call(
                &other,
                &b,
                "POST",
                &path,
                answer_body(&questions["items"][0], "wrong")
            )
            .await
            .0,
            200
        );
        let mut lock = pool.begin().await.unwrap();
        let lock_sql = if wallet_wait {
            "SELECT owner_id FROM coin_wallets WHERE owner_id=$1 FOR UPDATE"
        } else {
            "SELECT id FROM users WHERE id=$1 FOR UPDATE"
        };
        sqlx::query(lock_sql)
            .bind(b.subject)
            .execute(&mut *lock)
            .await
            .unwrap();
        let pending = call(
            &other,
            &b,
            "POST",
            &path,
            answer_body(&questions["items"][1], "wrong"),
        );
        let release = async {
            tokio::time::timeout(std::time::Duration::from_secs(5),async {
                loop {
                    let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')").fetch_one(&pool).await.unwrap();
                    if waiting {break;}
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            sqlx::query("UPDATE reward_clock.instant SET at=$1")
                .bind(after.parse::<chrono::DateTime<chrono::Utc>>().unwrap())
                .execute(&pool)
                .await
                .unwrap();
            lock.commit().await.unwrap();
        };
        let (r, ()) = tokio::join!(pending, release);
        assert_eq!(r.0, 409, "{}", r.1);
        assert_eq!(issuance(&pool).await, (2, 34));
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM learning_completions WHERE run_id=$1")
                .bind(Uuid::parse_str(run["id"].as_str().unwrap()).unwrap())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
    }
    timed.close().await;
}
