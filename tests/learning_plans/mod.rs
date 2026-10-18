use crate::{account_deletion_support::call, learning_tasks_support::*};
use serde_json::{Value, json};
use sqlx::PgPool;
use tsz_rust::{auth::extract::AuthUser, state::AppState};
use uuid::Uuid;
async fn fixture(pool: &PgPool, count: usize) -> (AppState, AuthUser, Uuid) {
    let (state, auth, list) = setup(pool).await;
    let entry: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(pool)
            .await
            .unwrap();
    let publication: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id=$1")
            .bind(entry)
            .fetch_one(pool)
            .await
            .unwrap();
    let mut snapshot: Value =
        sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id=$1")
            .bind(publication)
            .fetch_one(pool)
            .await
            .unwrap();
    let base = snapshot["meanings"]["pos"][0]["senses"][0].clone();
    snapshot["meanings"]["pos"][0]["senses"] = json!(
        (0..count)
            .map(|i| {
                let mut sense = base.clone();
                sense["id"] = json!(Uuid::now_v7());
                sense["definitions"][0]["id"] = json!(Uuid::now_v7());
                sense["definitions"][0]["content"]["text"] = json!(format!("第{i}个中文释义"));
                sense
            })
            .collect::<Vec<_>>()
    );
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=$1")
        .bind(publication)
        .bind(snapshot)
        .execute(pool)
        .await
        .unwrap();
    (state, auth, list)
}
fn preview_body(list: Uuid, n: Option<i32>) -> Value {
    json!({"task_type":if n.is_some(){"daily"}else{"longterm"},"wordlist_ids":[list],"daily_question_count":n,"schedule":null})
}
async fn create_plan(
    state: &AppState,
    auth: &AuthUser,
    list: Uuid,
    n: Option<i32>,
) -> (Value, Value) {
    let mut body = preview_body(list, n);
    let p = call(
        state,
        auth,
        "POST",
        "/api/v1/me/learning-plans/preview",
        body.clone(),
    )
    .await;
    assert_eq!(p.0, 200, "{}", p.1);
    body["name"] = json!("覆盖计划");
    body["idempotency_key"] = json!(Uuid::now_v7());
    body["preview_fingerprint"] = p.1["preview_fingerprint"].clone();
    let r = call(
        state,
        auth,
        "POST",
        "/api/v1/me/learning-plans",
        body.clone(),
    )
    .await;
    assert_eq!(r.0, 200, "{}", r.1);
    (r.1, body)
}
async fn open(state: &AppState, auth: &AuthUser, plan: &Value) -> Value {
    let r = call(
        state,
        auth,
        "POST",
        &format!(
            "/api/v1/me/learning-plans/{}/runs",
            plan["task"]["id"].as_str().unwrap()
        ),
        start_body(None),
    )
    .await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
async fn answer_n(state: &AppState, auth: &AuthUser, r: &Value, n: usize) {
    let mut remaining = n;
    for page in 1..=100 {
        let q = call(
            state,
            auth,
            "GET",
            &format!(
                "/api/v1/me/learning-runs/{}/questions?page={page}&page_size=50",
                r["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(q.0, 200, "{}", q.1);
        for item in q.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|q| q["answered"] == false && q["content_available"] == true)
        {
            if remaining == 0 {
                return;
            }
            let a = call(
                state,
                auth,
                "POST",
                &answer_path(r),
                answer_body(item, "错误答案"),
            )
            .await;
            assert_eq!(a.0, 200, "{}", a.1);
            remaining -= 1;
        }
        if remaining == 0 {
            return;
        }
    }
    panic!("not enough questions");
}
async fn get(state: &AppState, auth: &AuthUser, p: &Value) -> Value {
    let r = call(
        state,
        auth,
        "GET",
        &format!(
            "/api/v1/me/learning-plans/{}",
            p["task"]["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
async fn previous_day(pool: &PgPool, p: &Value) {
    let id: Uuid = p["task"]["id"].as_str().unwrap().parse().unwrap();
    sqlx::query("UPDATE learning_runs SET business_day=business_day-1,window_start=window_start-interval '1 day',window_end=window_end-interval '1 day',expires_at=expires_at-interval '1 day' WHERE task_id=$1").bind(id).execute(pool).await.unwrap();
    sqlx::query("UPDATE learning_completions SET business_day=business_day-1,completed_at=completed_at-interval '1 day' WHERE task_id=$1").bind(id).execute(pool).await.unwrap();
    sqlx::query("UPDATE learning_cycles SET completed_at=completed_at-interval '1 day' WHERE task_id=$1 AND completed_at IS NOT NULL").bind(id).execute(pool).await.unwrap();
}
#[sqlx::test]
async fn coverage_tail_partial_days_and_persistent_receipts(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 235).await;
    let (p, body) = create_plan(&state, &auth, list, Some(100)).await;
    assert_eq!(p["current_cycle"]["total_count"], 235);
    assert_eq!(
        call(&state, &auth, "POST", "/api/v1/me/learning-plans", body)
            .await
            .1["task"]["id"],
        p["task"]["id"]
    );
    let legacy = call(
        &state,
        &auth,
        "GET",
        "/api/v1/me/learning-tasks",
        Value::Null,
    )
    .await;
    assert_eq!(legacy.0, 409);
    assert!(legacy.1.to_string().contains("刷新"));
    let r = open(&state, &auth, &p).await;
    assert_eq!(r["target_count"], 100);
    answer_n(&state, &auth, &r, 60).await;
    let progress = get(&state, &auth, &p).await;
    assert_eq!(progress["current_cycle"]["covered_count"], 60);
    assert_eq!(progress["today_available_count"], 40);
    assert_eq!(progress["estimated_learning_days"], 3);
    previous_day(&pool, &p).await;
    let r2 = open(&state, &auth, &p).await;
    assert_eq!(r2["target_count"], 100);
    answer_n(&state, &auth, &r2, 100).await;
    previous_day(&pool, &p).await;
    let r3 = open(&state, &auth, &p).await;
    assert_eq!(r3["target_count"], 75);
    answer_n(&state, &auth, &r3, 75).await;
    let done = get(&state, &auth, &p).await;
    assert_eq!(done["current_cycle"]["state"], "completed");
    assert_eq!(done["current_cycle"]["covered_count"], 235);
    let duplicates:i64=sqlx::query_scalar("SELECT count(*)-count(DISTINCT cycle_unit_id) FROM learning_answers WHERE cycle_unit_id IS NOT NULL").fetch_one(&pool).await.unwrap();
    assert_eq!(duplicates, 0);
    let path = format!(
        "/api/v1/me/learning-plans/{}/cycles",
        p["task"]["id"].as_str().unwrap()
    );
    let mut input = json!({"operation":"next","after_cycle_id":done["current_cycle"]["id"],"expected_revision":1});
    let preview = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/preview"),
        input.clone(),
    )
    .await;
    assert_eq!(preview.0, 200, "{}", preview.1);
    assert!(preview.1["earliest_day"].as_str().unwrap() > done["business_day"].as_str().unwrap());
    previous_day(&pool, &p).await;
    let preview = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/preview"),
        input.clone(),
    )
    .await;
    assert_eq!(preview.1["earliest_day"], done["business_day"]);
    input["preview_fingerprint"] = preview.1["preview"]["preview_fingerprint"].clone();
    input["idempotency_key"] = json!(Uuid::now_v7());
    let (a, b) = tokio::join!(
        call(&state, &auth, "POST", &path, input.clone()),
        call(&state, &auth, "POST", &path, input.clone())
    );
    assert_eq!(a.0, 200, "{}", a.1);
    assert_eq!(a.1["id"], b.1["id"]);
    input["operation"] = json!("restart_blocked");
    assert_eq!(call(&state, &auth, "POST", &path, input).await.0, 409);
    let mut tx = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../../migrations/20261018130000_learning_coverage.down.sql"
        ))
        .execute(&mut *tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM learning_cycles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[sqlx::test]
async fn plan_dates_fingerprint_and_short_tail(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 35).await;
    let mut body = preview_body(list, Some(100));
    let p = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/learning-plans/preview",
        body.clone(),
    )
    .await;
    let day = p.1["business_day"].clone();
    body["schedule"] = json!({"start_date":day,"end_date":day});
    let p = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/learning-plans/preview",
        body.clone(),
    )
    .await;
    assert_eq!(p.1["can_create"], true, "{}", p.1);
    assert_eq!(p.1["estimated_learning_days"], 1);
    body["name"] = json!("尾量任务");
    body["idempotency_key"] = json!(Uuid::now_v7());
    body["preview_fingerprint"] = p.1["preview_fingerprint"].clone();
    let created = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/learning-plans",
        body.clone(),
    )
    .await;
    assert_eq!(created.0, 200, "{}", created.1);
    let r = open(&state, &auth, &created.1).await;
    assert_eq!(r["target_count"], 35);
    answer_n(&state, &auth, &r, 35).await;
    assert_eq!(
        get(&state, &auth, &created.1).await["current_cycle"]["state"],
        "completed"
    );
    body["idempotency_key"] = json!(Uuid::now_v7());
    sqlx::query("UPDATE student_profiles SET english_variant='AmE' WHERE user_id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &auth, "POST", "/api/v1/me/learning-plans", body)
            .await
            .0,
        409
    );
}

#[sqlx::test]
async fn covered_source_loss_does_not_block_other_answers_and_pending_loss_keeps_total(
    pool: PgPool,
) {
    let (state, auth, list) = fixture(&pool, 2).await;
    let extra = crate::wordlists_support::full_entry(&pool, "banana").await;
    sqlx::query(
        "INSERT INTO wordlist_items(wordlist_id,item_id,entry_id,position) VALUES($1,$2,$2,1)",
    )
    .bind(list)
    .bind(extra)
    .execute(&pool)
    .await
    .unwrap();
    let (p, _) = create_plan(&state, &auth, list, Some(4)).await;
    let r = open(&state, &auth, &p).await;
    let qs = questions(&state, &auth, &r).await;
    let first = &qs["items"][0];
    let answered = call(
        &state,
        &auth,
        "POST",
        &answer_path(&r),
        answer_body(first, "wrong"),
    )
    .await;
    assert_eq!(answered.0, 200, "{}", answered.1);
    let entry: Uuid = sqlx::query_scalar("SELECT entry_id FROM learning_questions WHERE id=$1")
        .bind(first["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=$1")
        .bind(entry)
        .execute(&pool)
        .await
        .unwrap();
    let d = get(&state, &auth, &p).await;
    assert_eq!(d["current_cycle"]["total_count"], 4);
    assert_eq!(d["current_cycle"]["covered_count"], 1);
    assert_eq!(d["current_cycle"]["blocked_count"], 1);
    assert!(d["estimated_learning_days"].is_null());
    answer_n(&state, &auth, &r, 2).await;
    let d = get(&state, &auth, &p).await;
    assert_eq!(d["current_cycle"]["covered_count"], 3);
    assert_eq!(d["current_run"]["state"], "active");
    assert!(d["current_run"]["completion_id"].is_null());
    let invisible = questions(&state, &auth, &r).await;
    assert_eq!(invisible["items"][0]["answered"], true);
    assert!(invisible["items"][0]["prompt"].is_null());
    sqlx::query("UPDATE lexicon.entries SET archived_at=NULL WHERE id=$1")
        .bind(entry)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&state, &auth, &p).await["current_cycle"]["blocked_count"],
        1
    );
    let path = format!(
        "/api/v1/me/learning-plans/{}/cycles",
        p["task"]["id"].as_str().unwrap()
    );
    let mut input = json!({"operation":"restart_blocked","after_cycle_id":d["current_cycle"]["id"],"expected_revision":1});
    let preview = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/preview"),
        input.clone(),
    )
    .await;
    assert_eq!(preview.0, 200, "{}", preview.1);
    assert!(preview.1["earliest_day"].as_str().unwrap() > d["business_day"].as_str().unwrap());
    input["preview_fingerprint"] = preview.1["preview"]["preview_fingerprint"].clone();
    input["idempotency_key"] = json!(Uuid::now_v7());
    let new = call(&state, &auth, "POST", &path, input.clone()).await;
    assert_eq!(new.0, 200, "{}", new.1);
    assert_eq!(new.1["covered_count"], 0);
    assert_eq!(
        call(&state, &auth, "POST", &path, input).await.1["id"],
        new.1["id"]
    );
    let blocked = call(
        &state,
        &auth,
        "POST",
        &format!(
            "/api/v1/me/learning-plans/{}/runs",
            p["task"]["id"].as_str().unwrap()
        ),
        start_body(None),
    )
    .await;
    assert_eq!(blocked.0, 409);
}

#[sqlx::test]
async fn final_current_question_permission_check_rolls_back_answer_and_cycle(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 1).await;
    let (p, _) = create_plan(&state, &auth, list, None).await;
    let r = open(&state, &auth, &p).await;
    sqlx::raw_sql("CREATE FUNCTION test_revoke_answer() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=(SELECT entry_id FROM learning_questions WHERE id=NEW.question_id); RETURN NEW; END $$; CREATE TRIGGER test_revoke_answer AFTER INSERT ON learning_answers FOR EACH ROW EXECUTE FUNCTION test_revoke_answer();").execute(&pool).await.unwrap();
    let qs = questions(&state, &auth, &r).await;
    let a = call(
        &state,
        &auth,
        "POST",
        &answer_path(&r),
        answer_body(&qs["items"][0], "wrong"),
    )
    .await;
    assert_eq!(a.0, 409, "{}", a.1);
    let d = get(&state, &auth, &p).await;
    assert_eq!(d["current_cycle"]["covered_count"], 0);
    assert_eq!(d["current_cycle"]["state"], "active");
    assert!(d["current_run"]["completion_id"].is_null());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM learning_answers")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn deletion_grace_completion_archives_and_cancels_remaining_cycle(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 2).await;
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let (p, _) = create_plan(&state, &auth, list, Some(1)).await;
    let r = open(&state, &auth, &p).await;
    sqlx::query("INSERT INTO wordlist_deletion_events(id,wordlist_id,deleted_at,grace_until,prior_state,public_generation) VALUES($1,$2,clock_timestamp(),clock_timestamp()+interval '7 days','published',0)").bind(Uuid::now_v7()).bind(list).execute(&pool).await.unwrap();
    sqlx::query("UPDATE wordlists SET deleted_at=clock_timestamp(),state='draft' WHERE id=$1")
        .bind(list)
        .execute(&pool)
        .await
        .unwrap();
    let d = get(&state, &auth, &p).await;
    assert_eq!(d["current_cycle"]["blocked_count"], 1);
    assert_eq!(d["can_start"], true);
    answer_n(&state, &auth, &r, 1).await;
    let d = get(&state, &auth, &p).await;
    assert_eq!(d["task"]["state"], "archived");
    assert_eq!(d["current_cycle"]["state"], "cancelled");
    assert_eq!(d["current_cycle"]["covered_count"], 1);
    assert_eq!(d["current_run"]["state"], "completed");
    assert_eq!(d["can_start"], false);
}

#[sqlx::test]
async fn directory_selection_expands_deduplicates_and_requires_current_version(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 1).await;
    let path = "/api/v1/me/wordlist-library/owned";
    let listing = call(&state, &auth, "GET", path, Value::Null).await;
    assert_eq!(listing.0, 200, "{}", listing.1);
    let folder = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/folders"),
        json!({"expected_version":listing.1["version"],"parent_id":null,"name":"学习目录"}),
    )
    .await;
    assert_eq!(folder.0, 204, "{}", folder.1);
    let folder: Uuid = sqlx::query_scalar(
        "SELECT id FROM wordlist_library_nodes WHERE user_id=$1 AND wordlist_id IS NULL",
    )
    .bind(auth.subject)
    .fetch_one(&pool)
    .await
    .unwrap();
    let child:Uuid=sqlx::query_scalar("UPDATE wordlist_library_nodes SET parent_id=$2,revision=revision+1 WHERE wordlist_id=$1 AND user_id=$3 RETURNING id").bind(list).bind(folder).bind(auth.subject).fetch_one(&pool).await.unwrap();
    let stale = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/selection"),
        json!({"expected_version":listing.1["version"],"node_ids":[folder,child]}),
    )
    .await;
    assert_eq!(stale.0, 409);
    let listing = call(&state, &auth, "GET", path, Value::Null).await;
    let chosen = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/selection"),
        json!({"expected_version":listing.1["version"],"node_ids":[folder,child]}),
    )
    .await;
    assert_eq!(chosen.0, 200, "{}", chosen.1);
    assert_eq!(chosen.1["wordlists"].as_array().unwrap().len(), 1);
    assert_eq!(chosen.1["wordlists"][0]["id"], list.to_string());
}

#[sqlx::test]
async fn zero_answer_restart_can_start_today_and_cannot_reset_used_quota(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 2).await;
    let (p, _) = create_plan(&state, &auth, list, Some(1)).await;
    let entry: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        "UPDATE wordlist_items SET learning_membership_id=gen_random_uuid() WHERE wordlist_id=$1",
    )
    .bind(list)
    .execute(&pool)
    .await
    .unwrap();
    let path = format!(
        "/api/v1/me/learning-plans/{}/cycles",
        p["task"]["id"].as_str().unwrap()
    );
    let mut input = json!({"operation":"restart_blocked","after_cycle_id":p["current_cycle"]["id"],"expected_revision":1});
    let preview = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/preview"),
        input.clone(),
    )
    .await;
    assert_eq!(preview.0, 200, "{}", preview.1);
    assert_eq!(preview.1["earliest_day"], p["business_day"]);
    input["preview_fingerprint"] = preview.1["preview"]["preview_fingerprint"].clone();
    input["idempotency_key"] = json!(Uuid::now_v7());
    let result = call(&state, &auth, "POST", &path, input).await;
    assert_eq!(result.0, 200, "{}", result.1);
    let r = open(&state, &auth, &p).await;
    answer_n(&state, &auth, &r, 1).await;
    // Replacing a membership invalidates only remaining units; it never erases today's completion.
    sqlx::query("UPDATE wordlist_items SET learning_membership_id=gen_random_uuid() WHERE wordlist_id=$1 AND entry_id=$2").bind(list).bind(entry).execute(&pool).await.unwrap();
    let d = get(&state, &auth, &p).await;
    let preview=call(&state,&auth,"POST",&format!("{path}/preview"),json!({"operation":"restart_blocked","after_cycle_id":d["current_cycle"]["id"],"expected_revision":1})).await;
    assert_eq!(preview.0, 200, "{}", preview.1);
    assert!(preview.1["earliest_day"].as_str().unwrap() > p["business_day"].as_str().unwrap());
}

async fn controlled_clock(pool: &PgPool) -> PgPool {
    sqlx::raw_sql("CREATE SCHEMA plan_clock; CREATE TABLE plan_clock.instant(at timestamptz NOT NULL); INSERT INTO plan_clock.instant VALUES('2026-10-17T06:00:00Z'); CREATE FUNCTION plan_clock.clock_timestamp() RETURNS timestamptz LANGUAGE sql VOLATILE AS 'SELECT at FROM plan_clock.instant';").execute(pool).await.unwrap();
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .after_connect(|c, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO plan_clock,public,pg_catalog")
                    .execute(c)
                    .await?;
                Ok(())
            })
        })
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap()
}
#[sqlx::test]
async fn full_round_has_100_100_35_without_cross_day_duplicates(pool: PgPool) {
    let timed = controlled_clock(&pool).await;
    let (state, auth, list) = fixture(&timed, 235).await;
    let (p, _) = create_plan(&state, &auth, list, Some(100)).await;
    for (day, count) in [(17, 100), (18, 100), (19, 35)] {
        sqlx::query("UPDATE plan_clock.instant SET at=$1::text::timestamptz")
            .bind(format!("2026-10-{day}T06:00:00Z"))
            .execute(&timed)
            .await
            .unwrap();
        let r = open(&state, &auth, &p).await;
        assert_eq!(r["target_count"], count);
        assert_eq!(open(&state, &auth, &p).await["id"], r["id"]);
        answer_n(&state, &auth, &r, count as usize).await;
        assert_eq!(open(&state, &auth, &p).await["id"], r["id"]);
    }
    let d = get(&state, &auth, &p).await;
    assert_eq!(d["current_cycle"]["covered_count"], 235);
    assert_eq!(d["current_cycle"]["state"], "completed");
    timed.close().await;
}
#[sqlx::test]
async fn partial_previous_day_coverage_never_becomes_reward_credit_and_short_tail_completes(
    pool: PgPool,
) {
    let timed = controlled_clock(&pool).await;
    sqlx::raw_sql(include_str!("../../ops/enable-learning-rewards.sql"))
        .execute(&timed)
        .await
        .unwrap();
    sqlx::query("UPDATE plan_clock.instant SET at='2026-10-18T06:00:00Z'")
        .execute(&timed)
        .await
        .unwrap();
    let (state, auth, list) = fixture(&timed, 15).await;
    let (p, _) = create_plan(&state, &auth, list, Some(10)).await;
    let r = open(&state, &auth, &p).await;
    answer_n(&state, &auth, &r, 6).await;
    let reward = call(
        &state,
        &auth,
        "GET",
        "/api/v1/me/coins/learning-rewards",
        Value::Null,
    )
    .await;
    assert_eq!(reward.1["qualifying_units"], 0);
    sqlx::query("UPDATE plan_clock.instant SET at='2026-10-19T06:00:00Z'")
        .execute(&timed)
        .await
        .unwrap();
    let tail = open(&state, &auth, &p).await;
    assert_eq!(tail["target_count"], 9);
    answer_n(&state, &auth, &tail, 9).await;
    assert_eq!(
        get(&state, &auth, &p).await["current_cycle"]["state"],
        "completed"
    );
    let reward = call(
        &state,
        &auth,
        "GET",
        "/api/v1/me/coins/learning-rewards",
        Value::Null,
    )
    .await;
    assert_eq!(reward.1["qualifying_units"], 9);
    assert_eq!(reward.1["status"], "in_progress");
    assert_eq!(reward.1["awarded_amount"], "0");
    let (p2, _) = create_plan(&state, &auth, list, Some(1)).await;
    let r = open(&state, &auth, &p2).await;
    answer_n(&state, &auth, &r, 1).await;
    let reward = call(
        &state,
        &auth,
        "GET",
        "/api/v1/me/coins/learning-rewards",
        Value::Null,
    )
    .await;
    assert_eq!(reward.1["qualifying_units"], 10);
    assert_eq!(reward.1["awarded_amount"], "5");
    timed.close().await;
}

#[sqlx::test]
async fn optional_schedule_gates_start_and_end_and_rechecks_last_answer_time(pool: PgPool) {
    let timed = controlled_clock(&pool).await;
    let (state, auth, list) = fixture(&timed, 2).await;
    let mut body = preview_body(list, Some(2));
    body["schedule"] = json!({"start_date":"2026-10-18","end_date":"2026-10-18"});
    let preview = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/learning-plans/preview",
        body.clone(),
    )
    .await;
    assert_eq!(preview.1["can_create"], true);
    body["name"] = json!("边界计划");
    body["idempotency_key"] = json!(Uuid::now_v7());
    body["preview_fingerprint"] = preview.1["preview_fingerprint"].clone();
    let p = call(&state, &auth, "POST", "/api/v1/me/learning-plans", body).await;
    assert_eq!(p.0, 200, "{}", p.1);
    assert_eq!(p.1["status"], "not_started");
    let path = format!(
        "/api/v1/me/learning-plans/{}/runs",
        p.1["task"]["id"].as_str().unwrap()
    );
    assert_eq!(
        call(&state, &auth, "POST", &path, start_body(None)).await.0,
        409
    );
    sqlx::query("UPDATE plan_clock.instant SET at='2026-10-18T19:59:59Z'")
        .execute(&timed)
        .await
        .unwrap();
    let r = open(&state, &auth, &p.1).await;
    let qs = questions(&state, &auth, &r).await;
    let first = answer_body(&qs["items"][0], "wrong");
    assert_eq!(
        call(&state, &auth, "POST", &answer_path(&r), first.clone())
            .await
            .0,
        200
    );
    sqlx::raw_sql("CREATE FUNCTION test_cross_plan_end() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN UPDATE plan_clock.instant SET at=at+interval '2 seconds'; RETURN NEW; END $$; CREATE TRIGGER test_cross_plan_end AFTER INSERT ON learning_answers FOR EACH ROW EXECUTE FUNCTION test_cross_plan_end();").execute(&timed).await.unwrap();
    let last = answer_body(&qs["items"][1], "wrong");
    assert_eq!(
        call(&state, &auth, "POST", &answer_path(&r), last.clone())
            .await
            .0,
        409
    );
    let d = get(&state, &auth, &p.1).await;
    assert_eq!(d["current_cycle"]["covered_count"], 1);
    assert!(d["current_run"]["completion_id"].is_null());
    sqlx::raw_sql("DROP TRIGGER test_cross_plan_end ON learning_answers; DROP FUNCTION test_cross_plan_end(); UPDATE plan_clock.instant SET at='2026-10-18T20:00:00Z';").execute(&timed).await.unwrap();
    assert_eq!(get(&state, &auth, &p.1).await["status"], "schedule_ended");
    assert_eq!(
        call(&state, &auth, "POST", &path, start_body(None)).await.0,
        409
    );
    assert_eq!(
        call(&state, &auth, "POST", &answer_path(&r), last).await.0,
        409
    );
    assert_eq!(
        call(&state, &auth, "POST", &answer_path(&r), first).await.0,
        200
    );
    timed.close().await;
}

#[sqlx::test]
async fn blocked_source_can_shorten_a_future_daily_batch_without_completing_the_cycle(
    pool: PgPool,
) {
    let timed = controlled_clock(&pool).await;
    let (state, auth, list) = fixture(&timed, 2).await;
    let blocked_entry: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&timed)
            .await
            .unwrap();
    let extra = crate::wordlists_support::full_entry(&timed, "banana").await;
    sqlx::query(
        "INSERT INTO wordlist_items(wordlist_id,item_id,entry_id,position) VALUES($1,$2,$2,1)",
    )
    .bind(list)
    .bind(extra)
    .execute(&timed)
    .await
    .unwrap();
    let (p, _) = create_plan(&state, &auth, list, Some(3)).await;
    let first = open(&state, &auth, &p).await;
    assert_eq!(first["target_count"], 3);
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=$1")
        .bind(blocked_entry)
        .execute(&timed)
        .await
        .unwrap();
    answer_n(&state, &auth, &first, 1).await;
    let d = get(&state, &auth, &p).await;
    assert_eq!(d["current_run"]["target_count"], 3);
    assert!(d["current_run"]["completion_id"].is_null());
    sqlx::query("UPDATE plan_clock.instant SET at='2026-10-18T06:00:00Z'")
        .execute(&timed)
        .await
        .unwrap();
    let next = open(&state, &auth, &p).await;
    assert_eq!(next["target_count"], 1);
    answer_n(&state, &auth, &next, 1).await;
    let d = get(&state, &auth, &p).await;
    assert_eq!(d["current_run"]["state"], "completed");
    assert_eq!(d["current_cycle"]["state"], "active");
    assert_eq!(d["current_cycle"]["total_count"], 4);
    assert_eq!(d["current_cycle"]["covered_count"], 2);
    assert_eq!(d["current_cycle"]["blocked_count"], 2);
    assert!(d["estimated_learning_days"].is_null());
    timed.close().await;
}

#[sqlx::test]
async fn existing_plan_metadata_survives_a_large_source_append(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 2).await;
    let (p, _) = create_plan(&state, &auth, list, Some(1)).await;
    let legacy = create(&state, &auth, list, "longterm").await;
    let entry: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(&pool)
            .await
            .unwrap();
    let metadata: Value =
        sqlx::query_scalar("SELECT to_jsonb(e) FROM lexicon.entries e WHERE id=$1")
            .bind(entry)
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut tx = pool.begin().await.unwrap();
    for i in 1..=1000 {
        let id = Uuid::now_v7();
        let mut row = metadata.clone();
        row["id"] = json!(id);
        row["current_publication_id"] = Value::Null;
        row["draft_based_on_publication_id"] = Value::Null;
        sqlx::query("INSERT INTO lexicon.entries SELECT * FROM jsonb_populate_record(NULL::lexicon.entries,$1)").bind(row).execute(&mut *tx).await.unwrap();
        sqlx::query(
            "INSERT INTO wordlist_items(wordlist_id,item_id,entry_id,position) VALUES($1,$2,$2,$3)",
        )
        .bind(list)
        .bind(id)
        .bind(i)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    let detail = get(&state, &auth, &p).await;
    assert_eq!(detail["current_cycle"]["total_count"], 2);
    assert_eq!(detail["sources"][0]["item_count"], 1001);
    let listing = call(
        &state,
        &auth,
        "GET",
        "/api/v1/me/learning-plans",
        Value::Null,
    )
    .await;
    assert_eq!(listing.0, 200, "{}", listing.1);
    assert_eq!(listing.1["items"].as_array().unwrap().len(), 2);
    let old = call(
        &state,
        &auth,
        "GET",
        &format!(
            "/api/v1/me/learning-plans/{}",
            legacy["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(old.0, 200, "{}", old.1);
    let r = open(&state, &auth, &p).await;
    assert_eq!(r["target_count"], 1);
}

#[sqlx::test]
async fn legacy_list_rechecks_policy_after_an_earlier_guard_transaction(pool: PgPool) {
    let (state, auth, list) = fixture(&pool, 2).await;
    tsz_rust::learning_tasks::plans::require_legacy(&pool, &auth, None)
        .await
        .unwrap();
    let _ = create_plan(&state, &auth, list, Some(1)).await;
    let query = tsz_rust::learning_tasks::dto::LearningPageQuery {
        page: None,
        page_size: None,
        state: None,
    };
    let result = tsz_rust::learning_tasks::service::list(&pool, &auth, query).await;
    assert!(
        result.is_err(),
        "a coverage task created after the earlier guard must not be encoded as a legacy list item"
    );
}

#[sqlx::test]
async fn cycle_candidate_lock_wait_cannot_cross_the_last_scheduled_day(pool: PgPool) {
    let timed = controlled_clock(&pool).await;
    let (state, auth, list) = fixture(&timed, 2).await;
    let mut body = preview_body(list, Some(10));
    body["schedule"] = json!({"start_date":"2026-10-17","end_date":"2026-10-18"});
    let p = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/learning-plans/preview",
        body.clone(),
    )
    .await;
    body["name"] = json!("续轮边界");
    body["idempotency_key"] = json!(Uuid::now_v7());
    body["preview_fingerprint"] = p.1["preview_fingerprint"].clone();
    let created = call(&state, &auth, "POST", "/api/v1/me/learning-plans", body).await;
    assert_eq!(created.0, 200, "{}", created.1);
    let p = created.1;
    let run = open(&state, &auth, &p).await;
    answer_n(&state, &auth, &run, 2).await;
    let extra = crate::wordlists_support::full_entry(&timed, "banana").await;
    sqlx::query(
        "INSERT INTO wordlist_items(wordlist_id,item_id,entry_id,position) VALUES($1,$2,$2,1)",
    )
    .bind(list)
    .bind(extra)
    .execute(&timed)
    .await
    .unwrap();
    let path = format!(
        "/api/v1/me/learning-plans/{}/cycles",
        p["task"]["id"].as_str().unwrap()
    );
    let input =
        json!({"operation":"next","after_cycle_id":p["current_cycle"]["id"],"expected_revision":1});
    for create in [true, false] {
        sqlx::query("UPDATE plan_clock.instant SET at='2026-10-18T19:59:59Z'")
            .execute(&timed)
            .await
            .unwrap();
        let preview = call(
            &state,
            &auth,
            "POST",
            &format!("{path}/preview"),
            input.clone(),
        )
        .await;
        assert_eq!(preview.0, 200, "{}", preview.1);
        let mut request = input.clone();
        let url = if create {
            request["idempotency_key"] = json!(Uuid::now_v7());
            request["preview_fingerprint"] = preview.1["preview"]["preview_fingerprint"].clone();
            path.clone()
        } else {
            format!("{path}/preview")
        };
        let mut blocker = timed.begin().await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        sqlx::query("SELECT id FROM lexicon.entries WHERE id=$1 FOR UPDATE")
            .bind(extra)
            .execute(&mut *blocker)
            .await
            .unwrap();
        let release = async {
            tokio::time::timeout(std::time::Duration::from_secs(5),async{
    loop{
     let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity a WHERE a.datname=current_database() AND $1=ANY(pg_blocking_pids(a.pid)))").bind(pid).fetch_one(&timed).await.unwrap();
     if waiting{break;}
     tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
   }).await.unwrap();
            sqlx::query("UPDATE plan_clock.instant SET at='2026-10-18T20:00:00Z'")
                .execute(&timed)
                .await
                .unwrap();
            blocker.commit().await.unwrap();
        };
        let (result, ()) = tokio::join!(call(&state, &auth, "POST", &url, request), release);
        assert_eq!(result.0, 409, "create={create}: {}", result.1);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM learning_cycles")
            .fetch_one(&timed)
            .await
            .unwrap();
        assert_eq!(count, 1);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM learning_cycle_requests")
            .fetch_one(&timed)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
    timed.close().await;
}
