mod account_deletion_support;
mod learning_tasks_support;
mod wordlists_support;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::{auth::extract::AuthUser, state::AppState};
use uuid::Uuid;
struct Fixture {
    state: AppState,
    auth: AuthUser,
    list: Uuid,
    entries: Vec<Uuid>,
}
async fn fixture(pool: &PgPool) -> Fixture {
    let (state, auth, list) = learning_tasks_support::setup(pool).await;
    let target = sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
        .bind(list)
        .fetch_one(pool)
        .await
        .unwrap();
    let mut entries = vec![target];
    for word in ["banana", "orange", "tomato"] {
        entries.push(wordlists_support::full_entry(pool, word).await);
    }
    for (i, id) in entries.iter().enumerate() {
        let mut snapshot:Value=sqlx::query_scalar("SELECT p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(id).fetch_one(pool).await.unwrap();
        for side in ["uk", "us"] {
            snapshot["forms"]["pos"][0]["forms"][1]["regional_variants"][side]["spelling"] =
                json!(format!("alternate{i}"));
        }
        snapshot["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] =
            json!(["苹果", "香蕉", "橙子", "番茄"][i]);
        sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(id).bind(snapshot).execute(pool).await.unwrap();
    }
    Fixture {
        state,
        auth,
        list,
        entries,
    }
}
async fn call(f: &Fixture, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let token = f
        .state
        .token_manager
        .generate_with_version(f.auth.subject, "student", 0)
        .unwrap();
    let response = tsz_rust::router(f.state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .header("X-TSZ-Learning-Capabilities", "choice-v1")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn preview(f: &Fixture) -> Value {
    json!({"task_type":"daily","wordlist_ids":[f.list],"daily_question_count":1,"schedule":null,"question_type":"choice_zh_to_en"})
}
async fn plan(f: &Fixture) -> Value {
    let previewed = call(f, "POST", "/api/v1/me/learning-plans/preview", preview(f)).await;
    assert_eq!(previewed.0, 200, "{}", previewed.1);
    assert_eq!(previewed.1["can_create"], true, "{}", previewed.1);
    let mut body = preview(f);
    body["preview_fingerprint"] = previewed.1["preview_fingerprint"].clone();
    body["name"] = json!("按需单选");
    body["idempotency_key"] = json!(Uuid::now_v7());
    let r = call(f, "POST", "/api/v1/me/learning-plans", body).await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
async fn open(f: &Fixture, p: &Value) -> Value {
    let r=call(f,"POST",&format!("/api/v1/me/learning-plans/{}/runs",p["task"]["id"].as_str().unwrap()),json!({"idempotency_key":Uuid::now_v7(),"expected_revision":p["task"]["revision"],"after_run_id":null})).await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
async fn questions(f: &Fixture, r: &Value) -> Value {
    let r = call(
        f,
        "GET",
        &format!(
            "/api/v1/me/learning-runs/{}/questions",
            r["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
#[sqlx::test]
async fn empty_bank_generates_only_when_run_opens_and_restores_grading_snapshots(pool: PgPool) {
    let f = fixture(&pool).await;
    let r = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(r.0, 200);
    assert_eq!(r.1["can_create"], true);
    let p = plan(&f).await;
    let deferred: Value =
        sqlx::query_scalar("SELECT answer_snapshot FROM learning_cycle_units LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(deferred["choice"]["deferred"], true);
    assert_eq!(deferred["choice"]["content"]["options"], json!([]));
    let run = open(&f, &p).await;
    let qs = questions(&f, &run).await;
    let q = &qs["items"][0];
    assert_eq!(q["options"].as_array().unwrap().len(), 4);
    assert!(q.get("correct_option_id").is_none());
    assert_eq!(q["feedback"], Value::Null);
    let stored: Value =
        sqlx::query_scalar("SELECT answer_snapshot FROM learning_questions LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored["choice"]["deferred"], false);
    assert_eq!(
        stored["choice"]["content"]["options"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let restored = questions(&f, &open(&f, &p).await).await;
    assert_eq!(restored, qs);
    let option = q["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["spelling"] == "apple")
        .unwrap()["option_id"]
        .as_str()
        .unwrap();
    assert!(Uuid::parse_str(option).is_ok());
    let path = format!(
        "/api/v1/me/learning-runs/{}/answers",
        run["id"].as_str().unwrap()
    );
    let body = learning_tasks_support::answer_body(q, option);
    let answer = call(&f, "POST", &path, body.clone()).await;
    assert_eq!(answer.0, 200, "{}", answer.1);
    assert_eq!(answer.1["is_correct"], true);
    assert_eq!(
        call(&f, "POST", &path, body).await.1["answer_id"],
        answer.1["answer_id"]
    );
    let finished = questions(&f, &run).await;
    assert_eq!(
        finished["items"][0]["feedback"]["correct_option_id"],
        option
    );
    let counts:(i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM question_bank),(SELECT count(*) FROM question_generations),(SELECT count(*) FROM learning_answers),(SELECT count(*) FROM learning_completions)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 1, 1));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_memory_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test]
async fn insufficient_distractors_exclude_targets_without_bank_fallback(pool: PgPool) {
    let f = fixture(&pool).await;
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=ANY($1)")
        .bind(&f.entries[2..])
        .execute(&pool)
        .await
        .unwrap();
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.0, 200);
    assert_eq!(p.1["can_create"], false);
    assert!(
        p.1["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap().contains("选项"))
    );
}
#[sqlx::test]
async fn partial_definition_overlap_is_not_a_valid_fourth_option(pool: PgPool) {
    let f = fixture(&pool).await;
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=jsonb_set(snapshot,'{meanings,pos,0,senses,0,definitions,0,content,text}',to_jsonb('苹果的果实'::text)) WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(f.entries[1]).execute(&pool).await.unwrap();
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.0, 200);
    assert_eq!(p.1["can_create"], false);
}
#[sqlx::test]
async fn source_withdrawal_hides_unanswered_choice_and_blocks_answers(pool: PgPool) {
    let f = fixture(&pool).await;
    let p = plan(&f).await;
    let run = open(&f, &p).await;
    let qs = questions(&f, &run).await;
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=$1")
        .bind(f.entries[1])
        .execute(&pool)
        .await
        .unwrap();
    let changed = questions(&f, &run).await;
    assert_eq!(changed["items"][0]["content_available"], false);
    assert_eq!(changed["items"][0]["prompt"], Value::Null);
    let result = call(
        &f,
        "POST",
        &format!(
            "/api/v1/me/learning-runs/{}/answers",
            run["id"].as_str().unwrap()
        ),
        learning_tasks_support::answer_body(
            &qs["items"][0],
            qs["items"][0]["options"][0]["option_id"].as_str().unwrap(),
        ),
    )
    .await;
    assert_eq!(result.0, 409);
}
#[sqlx::test]
async fn legacy_full_choice_snapshots_do_not_require_bank_records(pool: PgPool) {
    let f = fixture(&pool).await;
    let p = plan(&f).await;
    let run = open(&f, &p).await;
    sqlx::query(
        "UPDATE learning_questions SET answer_snapshot=answer_snapshot #- '{choice,deferred}'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let restored = questions(&f, &run).await;
    assert_eq!(restored["items"][0]["content_available"], true);
    assert_eq!(restored["items"][0]["options"].as_array().unwrap().len(), 4);
}
#[sqlx::test]
async fn retired_workbench_and_generation_routes_are_not_mounted(pool: PgPool) {
    let f = fixture(&pool).await;
    for (method, path) in [
        ("GET", "/api/v1/admin/question-bank/questions"),
        ("POST", "/api/v1/admin/question-bank/generations"),
        ("POST", "/api/v1/admin/question-bank/batches"),
        (
            "GET",
            "/api/internal/agent/v1/requests/00000000-0000-0000-0000-000000000001/context",
        ),
    ] {
        assert_eq!(call(&f, method, path, json!({})).await.0, 404, "{path}");
    }
}

#[sqlx::test]
async fn published_incoming_synonym_links_are_excluded_even_with_different_glosses(pool: PgPool) {
    let f = fixture(&pool).await;
    let relation = json!([{"id":Uuid::now_v7(),"relation":"synonym","target_word_id":f.entries[0],"score":"1"}]);
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=jsonb_set(snapshot,'{meanings,pos,0,senses,0,relations}',$2) WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(f.entries[1]).bind(relation).execute(&pool).await.unwrap();
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.0, 200, "{}", p.1);
    assert_eq!(p.1["can_create"], false);
}
#[sqlx::test]
async fn losing_distractors_before_open_does_not_commit_a_partial_run(pool: PgPool) {
    let f = fixture(&pool).await;
    let p = plan(&f).await;
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=$1")
        .bind(f.entries[1])
        .execute(&pool)
        .await
        .unwrap();
    let r = call(
        &f,
        "POST",
        &format!(
            "/api/v1/me/learning-plans/{}/runs",
            p["task"]["id"].as_str().unwrap()
        ),
        learning_tasks_support::start_body(None),
    )
    .await;
    assert_eq!(r.0, 409, "{}", r.1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=true WHERE id=$1")
        .bind(f.entries[1])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        questions(&f, &open(&f, &p).await).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[sqlx::test]
async fn retirement_preserves_old_facts_and_refuses_rollback_after_on_demand_cycles(pool: PgPool) {
    let f = fixture(&pool).await;
    let admin = wordlists_support::admin(&pool).await;
    let g = Uuid::now_v7();
    let q = Uuid::now_v7();
    sqlx::query("INSERT INTO question_generations(id,created_by,security_version,permission_version,request_key,request_hash,target,question_level,answer_profile) VALUES($1,$2,0,0,$1,'hash','{}','A1','BrE')").bind(g).bind(admin).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO question_bank(id,generation_id,state,content,original_content,target_entry_id,target_sense_id,target_publication_id,question_level,answer_profile,created_by,review_id,question_revision_id) VALUES($1,$2,'approved','{\"sentinel\":true}','{\"sentinel\":true}',$3,$1,$1,'A1','BrE',$4,$1,$1)").bind(q).bind(g).bind(f.entries[0]).bind(admin).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261030030000_retire_question_bank.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let row: (String, Value) =
        sqlx::query_as("SELECT state,content FROM question_bank WHERE id=$1")
            .bind(q)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, ("disabled".into(), json!({"sentinel":true})));
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM question_generations WHERE id=$1")
            .bind(g)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "cancelled"
    );
    sqlx::raw_sql(include_str!(
        "../migrations/20261030030000_retire_question_bank.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    plan(&f).await;
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261030030000_retire_question_bank.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM question_bank WHERE id=$1 AND state='disabled'"
        )
        .bind(q)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
}

#[sqlx::test]
async fn longterm_source_loss_cannot_copy_deferred_empty_questions_into_a_run(pool: PgPool) {
    let mut f = fixture(&pool).await;
    let grape = wordlists_support::full_entry(&pool, "grape").await;
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=jsonb_set(snapshot,'{meanings,pos,0,senses,0,definitions,0,content,text}',to_jsonb('葡萄'::text)) WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(grape).execute(&pool).await.unwrap();
    let list = account_deletion_support::call(
        &f.state,
        &f.auth,
        "POST",
        "/api/v1/me/wordlists",
        wordlists_support::create_body(&f.entries[..2]),
    )
    .await;
    assert_eq!(list.0, 200);
    f.list = Uuid::parse_str(list.1["id"].as_str().unwrap()).unwrap();
    let mut input = preview(&f);
    input["task_type"] = json!("longterm");
    input["daily_question_count"] = Value::Null;
    let previewed = call(
        &f,
        "POST",
        "/api/v1/me/learning-plans/preview",
        input.clone(),
    )
    .await;
    assert_eq!(previewed.1["eligible_count"], 2);
    input["preview_fingerprint"] = previewed.1["preview_fingerprint"].clone();
    input["name"] = json!("长任务按需");
    input["idempotency_key"] = json!(Uuid::now_v7());
    let p = call(&f, "POST", "/api/v1/me/learning-plans", input).await;
    assert_eq!(p.0, 200, "{}", p.1);
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=$1")
        .bind(f.entries[0])
        .execute(&pool)
        .await
        .unwrap();
    let path = format!(
        "/api/v1/me/learning-plans/{}/runs",
        p.1["task"]["id"].as_str().unwrap()
    );
    let r = call(&f, "POST", &path, learning_tasks_support::start_body(None)).await;
    assert_eq!(r.0, 409, "{}", r.1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=true WHERE id=$1")
        .bind(f.entries[0])
        .execute(&pool)
        .await
        .unwrap();
    let run = open(&f, &p.1).await;
    let qs = questions(&f, &run).await;
    assert_eq!(qs["items"].as_array().unwrap().len(), 2);
    assert!(
        qs["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["options"].as_array().unwrap().len() == 4)
    );
}
