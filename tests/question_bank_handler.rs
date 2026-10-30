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
        // These choice/mastery fixtures model one target per word; context fallback has its own plan regression.
        snapshot["meanings"]["pos"][0]["senses"]
            .as_array_mut()
            .unwrap()
            .truncate(1);
        for side in ["uk", "us"] {
            snapshot["forms"]["pos"][0]["forms"][1]["regional_variants"][side]["spelling"] =
                json!(format!("alternate{i}"));
        }
        snapshot["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] =
            json!(["苹果", "香蕉", "橙子", "番茄"][i]);
        sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(id).bind(snapshot).execute(pool).await.unwrap();
    }
    seed_semantics(pool).await;
    Fixture {
        state,
        auth,
        list,
        entries,
    }
}
async fn seed_semantics(pool: &PgPool) {
    let dataset:i64=sqlx::query_scalar("INSERT INTO dictionary.datasets(version,source_name,source_version,rules_version,terms_sha256,regions_sha256,status) VALUES('choice-fixture','Kaikki','fixture','fixture','hash','hash','active') RETURNING id").fetch_one(pool).await.unwrap();
    for (i, (word, zh, gloss)) in [
        ("apple", "苹果", "crisp red orchard fruit"),
        ("banana", "香蕉", "curved yellow edible fruit"),
        ("orange", "橙子", "citrus juicy round fruit"),
        ("tomato", "番茄", "red savoury salad fruit"),
    ]
    .iter()
    .enumerate()
    {
        for spelling in [word.to_string(), format!("alternate{i}")] {
            let sense = json!([{"id":format!("{word}-fruit"),"glosses":[gloss],"hypernyms":[{"word":"fruit"}],"translations":[{"code":"zh","word":zh}]}]);
            sqlx::query("INSERT INTO dictionary.entry_contents(dataset_id,source_key,normalized_term,pos,senses,source_locator) VALUES($1,$2,$2,'noun',$3,'owned-fixture')").bind(dataset).bind(spelling).bind(sense).execute(pool).await.unwrap();
        }
    }
}
async fn call(f: &Fixture, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    call_with_capabilities(
        f,
        method,
        path,
        body,
        "choice-v1,mastery-v1,wordlist-spelling-v1",
    )
    .await
}
async fn call_with_capabilities(
    f: &Fixture,
    method: &str,
    path: &str,
    body: Value,
    capabilities: &str,
) -> (StatusCode, Value) {
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
                .header("X-TSZ-Learning-Capabilities", capabilities)
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
async fn assert_spelling_fallback(f: &Fixture) {
    let p = plan(f).await;
    let run = open(f, &p).await;
    let page = questions(f, &run).await;
    let items = page["items"].as_array().unwrap();
    assert!(!items.is_empty());
    assert!(
        items
            .iter()
            .all(|q| q["content_available"] == true && q.get("options").is_none())
    );
}
#[sqlx::test]
async fn legacy_platform_learning_facts_survive_migration_roundtrip(pool: PgPool) {
    let f = fixture(&pool).await;
    let task = learning_tasks_support::create(&f.state, &f.auth, f.list, "longterm").await;
    let run = learning_tasks_support::start(&f.state, &f.auth, &task).await;
    let before = questions(&f, &run).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261030070000_wordlist_practice.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261030070000_wordlist_practice.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(questions(&f, &run).await, before);
    let q = &before["items"][0];
    let answered = call(
        &f,
        "POST",
        &learning_tasks_support::answer_path(&run),
        learning_tasks_support::answer_body(q, "apple"),
    )
    .await;
    assert_eq!(answered.0, 200, "{}", answered.1);
    assert_eq!(answered.1["is_correct"], true);
}
#[sqlx::test]
async fn platform_only_new_learning_facts_block_incompatible_rollback(pool: PgPool) {
    let f = fixture(&pool).await;
    let p = plan(&f).await;
    let down = include_str!("../migrations/20261030070000_wordlist_practice.down.sql");
    let mut tx = pool.begin().await.unwrap();
    let result = sqlx::raw_sql(down).execute(&mut *tx).await;
    tx.rollback().await.unwrap();
    assert!(
        result.is_err(),
        "new platform-only cycle must block rollback before a run exists"
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("new learning generation facts exist")
    );

    let run = open(&f, &p).await;
    let custom_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM learning_questions WHERE custom_item_id IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(custom_count, 0);
    // Isolate the run guard from the cycle guard, then restore both with the transaction.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE learning_cycles SET generation_version='choice_algorithm_v1',grading_version='choice_id_v1'")
        .execute(&mut *tx).await.unwrap();
    let result = sqlx::raw_sql(down).execute(&mut *tx).await;
    tx.rollback().await.unwrap();
    assert!(
        result.is_err(),
        "new platform-only run must independently block rollback"
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("new learning generation facts exist")
    );
    let page = questions(&f, &run).await;
    let q = &page["items"][0];
    let option = q["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["spelling"] == "apple")
        .unwrap()["option_id"]
        .as_str()
        .unwrap();
    let answered = call(
        &f,
        "POST",
        &learning_tasks_support::answer_path(&run),
        learning_tasks_support::answer_body(q, option),
    )
    .await;
    assert_eq!(answered.0, 200, "{}", answered.1);
    assert_eq!(answered.1["is_correct"], true);
}
#[sqlx::test]
async fn old_choice_client_gets_an_explicit_update_requirement_for_spelling_fallback(pool: PgPool) {
    let f = fixture(&pool).await;
    let rejected = call_with_capabilities(
        &f,
        "POST",
        "/api/v1/me/learning-plans/preview",
        preview(&f),
        "choice-v1",
    )
    .await;
    assert_eq!(rejected.0, 409);
    assert!(rejected.1.to_string().contains("更新"));
    let plan = plan(&f).await;
    let run = open(&f, &plan).await;
    let path = format!(
        "/api/v1/me/learning-runs/{}/questions",
        run["id"].as_str().unwrap()
    );
    assert_eq!(
        call_with_capabilities(&f, "GET", &path, Value::Null, "choice-v1")
            .await
            .0,
        409
    );
    assert_eq!(call(&f, "GET", &path, Value::Null).await.0, 200);
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
async fn insufficient_distractors_fall_back_to_spelling_without_bank_fallback(pool: PgPool) {
    let f = fixture(&pool).await;
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=ANY($1)")
        .bind(&f.entries[2..])
        .execute(&pool)
        .await
        .unwrap();
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.0, 200);
    assert_eq!(p.1["can_create"], true);
    let p = plan(&f).await;
    let run = open(&f, &p).await;
    let qs = questions(&f, &run).await;
    assert!(qs["items"][0].get("options").is_none());
    let answer = call(
        &f,
        "POST",
        &learning_tasks_support::answer_path(&run),
        learning_tasks_support::answer_body(&qs["items"][0], "apple"),
    )
    .await;
    assert_eq!(answer.0, 200, "{}", answer.1);
    assert_eq!(answer.1["is_correct"], true);
}
#[sqlx::test]
async fn choice_plan_keeps_platform_options_and_custom_spelling_with_persistent_progress(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    sqlx::query(
        "INSERT INTO wordlist_items(wordlist_id,item_id,custom_word,position) VALUES($1,$2,$3,1)",
    )
    .bind(f.list)
    .bind(Uuid::now_v7())
    .bind(json!({"word":"starlight","definition":"星光"}))
    .execute(&pool)
    .await
    .unwrap();
    let mut body = preview(&f);
    body["task_type"] = json!("longterm");
    body["daily_question_count"] = Value::Null;
    let preview = call(
        &f,
        "POST",
        "/api/v1/me/learning-plans/preview",
        body.clone(),
    )
    .await;
    assert_eq!(preview.0, 200, "{}", preview.1);
    assert_eq!(preview.1["eligible_count"], 2);
    body["name"] = json!("混合练习");
    body["preview_fingerprint"] = preview.1["preview_fingerprint"].clone();
    body["idempotency_key"] = json!(Uuid::now_v7());
    let plan = call(&f, "POST", "/api/v1/me/learning-plans", body).await;
    assert_eq!(plan.0, 200, "{}", plan.1);
    let run = open(&f, &plan.1).await;
    let qs = questions(&f, &run).await;
    for q in qs["items"].as_array().unwrap() {
        let answer = if let Some(options) = q["options"].as_array() {
            assert_eq!(options.len(), 4);
            options.iter().find(|o| o["spelling"] == "apple").unwrap()["option_id"]
                .as_str()
                .unwrap()
        } else {
            if q["prompt"]["definition"] == "星光" {
                "starlight"
            } else {
                "apple"
            }
        };
        let result = call(
            &f,
            "POST",
            &learning_tasks_support::answer_path(&run),
            learning_tasks_support::answer_body(q, answer),
        )
        .await;
        assert_eq!(result.0, 200, "{}", result.1);
        assert_eq!(result.1["is_correct"], true);
    }
    let refreshed = call(
        &f,
        "GET",
        &format!(
            "/api/v1/me/learning-plans/{}",
            plan.1["task"]["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(refreshed.1["current_cycle"]["covered_count"], 2);
    assert_eq!(refreshed.1["current_run"]["state"], "completed");
    let invalid:i64=sqlx::query_scalar("SELECT count(*) FROM learning_questions WHERE custom_item_id IS NOT NULL AND (entry_id IS NOT NULL OR pos_id IS NOT NULL)").fetch_one(&pool).await.unwrap();
    assert_eq!(invalid, 0);
}
#[sqlx::test]
async fn partial_definition_overlap_is_not_a_valid_fourth_option(pool: PgPool) {
    let f = fixture(&pool).await;
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=jsonb_set(snapshot,'{meanings,pos,0,senses,0,definitions,0,content,text}',to_jsonb('苹果的果实'::text)) WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(f.entries[1]).execute(&pool).await.unwrap();
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.0, 200);
    assert_eq!(p.1["can_create"], true);
    let p = plan(&f).await;
    let run = open(&f, &p).await;
    assert!(
        questions(&f, &run).await["items"][0]
            .get("options")
            .is_none()
    );
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
    assert_eq!(p.1["can_create"], true);
    let p = plan(&f).await;
    let run = open(&f, &p).await;
    assert!(
        questions(&f, &run).await["items"][0]
            .get("options")
            .is_none()
    );
}
#[sqlx::test]
async fn losing_distractors_before_open_falls_back_to_spelling(pool: PgPool) {
    let f = fixture(&pool).await;
    let p = plan(&f).await;
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=$1")
        .bind(f.entries[1])
        .execute(&pool)
        .await
        .unwrap();
    let run = open(&f, &p).await;
    let q = questions(&f, &run).await;
    assert_eq!(q["items"][0]["content_available"], true);
    assert!(q["items"][0].get("options").is_none());
    assert_eq!(open(&f, &p).await["id"], run["id"]);
    let answer = call(
        &f,
        "POST",
        &learning_tasks_support::answer_path(&run),
        learning_tasks_support::answer_body(&q["items"][0], "apple"),
    )
    .await;
    assert_eq!(answer.0, 200, "{}", answer.1);
    assert_eq!(answer.1["is_correct"], true);
}
#[sqlx::test]
async fn retirement_preserves_old_facts_and_refuses_rollback_after_on_demand_cycles(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../migrations/20261030030000_retire_question_bank.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
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
    assert!(qs["items"].as_array().unwrap().iter().all(|q| {
        q.get("options")
            .is_none_or(|o| o.as_array().is_some_and(|options| options.len() == 4))
    }));
}

// Real publication/index triggers stay enabled; these are owned synthetic corpus rows.
async fn seed_corpus(
    pool: &PgPool,
    template: Uuid,
    count: usize,
    entry_base: u128,
    publication_base: u128,
    prefix: &str,
    definition: &str,
) -> Vec<Uuid> {
    let (mut content,admin):(Value,Uuid)=sqlx::query_as("SELECT p.snapshot,e.created_by_admin_id FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(template).fetch_one(pool).await.unwrap();
    content["forms"]["pos"][0]["forms"] = json!([content["forms"]["pos"][0]["forms"][0].clone()]);
    let mut group = content["forms"]["pos"][0]["form_groups"][0].clone();
    group["members"] = json!([group["members"][0].clone()]);
    content["forms"]["pos"][0]["form_groups"] = json!([group]);
    content["meanings"]["pos"][0]["senses"] =
        json!([content["meanings"]["pos"][0]["senses"][0].clone()]);
    content["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] =
        json!(definition);
    let ids: Vec<_> = (0..count)
        .map(|i| Uuid::from_u128(entry_base + i as u128))
        .collect();
    let pubs: Vec<_> = (0..count)
        .map(|i| Uuid::from_u128(publication_base + i as u128))
        .collect();
    for (offset, chunk) in ids.chunks(250).enumerate() {
        let begin = offset * 250;
        let mut entries = sqlx::QueryBuilder::new(
            "INSERT INTO lexicon.entries(id,content_schema_version,language,kind,detection_snapshot,created_by_admin_id,updated_by_admin_id) ",
        );
        entries.push_values(chunk, |mut b, id| {
            b.push_bind(*id)
                .push("3")
                .push("'en'")
                .push("'word'")
                .push("'{}'")
                .push_bind(admin)
                .push_bind(admin);
        });
        entries.build().execute(pool).await.unwrap();
        let mut rows = Vec::new();
        for (j, id) in chunk.iter().enumerate() {
            let mut c = content.clone();
            let number = begin + j;
            let word = if prefix == "target" {
                // Spread targets over 676 prefixes rather than measuring a single hot prefix.
                format!(
                    "{}{}unit{number:04}",
                    char::from(b'a' + ((number / 26) % 26) as u8),
                    char::from(b'a' + (number % 26) as u8)
                )
            } else {
                format!("{prefix}{number:04}")
            };
            c["id"] = json!(id);
            c["presentation"]["label"] = json!(word);
            for side in ["uk", "us"] {
                c["forms"]["pos"][0]["forms"][0]["regional_variants"][side]["spelling"] =
                    json!(word);
            }
            rows.push((pubs[begin + j], *id, c));
        }
        let mut insert = sqlx::QueryBuilder::new(
            "INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) ",
        );
        insert.push_values(&rows, |mut b, (publication, id, c)| {
            b.push_bind(*publication)
                .push_bind(*id)
                .push("1")
                .push("2")
                .push("3")
                .push_bind(c.clone())
                .push_bind(publication.as_bytes().to_vec())
                .push_bind(admin);
        });
        insert.build().execute(pool).await.unwrap();
        sqlx::query("UPDATE lexicon.entries e SET current_publication_id=x.publication FROM unnest($1::uuid[],$2::uuid[])x(id,publication) WHERE e.id=x.id").bind(chunk).bind(&pubs[begin..begin+chunk.len()]).execute(pool).await.unwrap();
        let dataset: i64 =
            sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status='active'")
                .fetch_one(pool)
                .await
                .unwrap();
        let mut insert = sqlx::QueryBuilder::new(
            "INSERT INTO dictionary.entry_contents(dataset_id,source_key,normalized_term,pos,senses,source_locator) ",
        );
        insert.push_values(&rows,|mut b,(_,_,c)|{
            let word=c["presentation"]["label"].as_str().unwrap();
            let gloss=if definition=="目标" {"synthetic target meaning"} else {"synthetic alternative meaning"};
            b.push_bind(dataset).push_bind(word).push_bind(word).push("'noun'").push_bind(json!([{"glosses":[gloss],"hypernyms":[{"word":if prefix=="lookalike" {"fruit"}else{"fixture domain"}}],"synonyms":if prefix=="lookalike"{json!([{"word":"apple"}])}else{json!([])},"translations":[{"code":"zh","word":definition}]}])).push("'owned-scale-fixture'");
        });
        insert.build().execute(pool).await.unwrap();
    }
    ids
}
#[sqlx::test]
async fn paged_recall_continues_past_overlapping_entries(pool: PgPool) {
    let f = fixture(&pool).await;
    seed_corpus(&pool, f.entries[0], 80, 100, 10000, "lookalike", "相似物").await;
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.0, 200, "{}", p.1);
    assert!(p.1["eligible_count"].as_i64().unwrap() >= 1);
    assert_eq!(p.1["can_create"], true);
    let plan = plan(&f).await;
    let run = open(&f, &plan).await;
    let page = questions(&f, &run).await;
    assert_eq!(page["items"][0]["prompt"]["definition"], "苹果");
    assert_eq!(page["items"][0]["options"].as_array().unwrap().len(), 4);
}
#[sqlx::test]
#[ignore = "manual 50k publication/1000-target performance evidence; run explicitly before shipping changes to recall"]
async fn on_demand_scale_50k_candidates_1000_targets(pool: PgPool) {
    let mut f = fixture(&pool).await;
    let seed_started = std::time::Instant::now();
    seed_corpus(
        &pool,
        f.entries[0],
        50000,
        0x80000000000000000000000000000000,
        0x90000000000000000000000000000000,
        "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        "备用",
    )
    .await;
    let ids = seed_corpus(
        &pool,
        f.entries[0],
        1000,
        0x20000000000000000000000000000000,
        0x30000000000000000000000000000000,
        "target",
        "目标",
    )
    .await;
    let list = account_deletion_support::call(
        &f.state,
        &f.auth,
        "POST",
        "/api/v1/me/wordlists",
        wordlists_support::create_body(&ids),
    )
    .await;
    assert_eq!(list.0, 200, "{}", list.1);
    f.list = Uuid::parse_str(list.1["id"].as_str().unwrap()).unwrap();
    eprintln!("scale corpus prepared in {:?}", seed_started.elapsed());
    let request_started = std::time::Instant::now();
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    let elapsed = request_started.elapsed();
    assert_eq!(p.0, 200, "{}", p.1);
    assert_eq!(p.1["eligible_count"], 1000);
    assert_eq!(p.1["can_create"], true);
    eprintln!(
        "50k published distractors, 1000 target units: preview {:?}",
        elapsed
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM question_bank")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn semantic_relation_beats_spelling_only_neighbour_and_missing_evidence_does_not_fill(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let airport = wordlists_support::full_entry(&pool, "airport").await;
    let mut snapshot:Value=sqlx::query_scalar("SELECT p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(airport).fetch_one(&pool).await.unwrap();
    for side in ["uk", "us"] {
        snapshot["forms"]["pos"][0]["forms"][1]["regional_variants"][side]["spelling"] =
            json!("airport");
    }
    snapshot["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] =
        json!("机场");
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(airport).bind(snapshot).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO dictionary.entry_contents(dataset_id,source_key,normalized_term,pos,senses,source_locator) SELECT id,'airport','airport','noun',$1,'owned-fixture' FROM dictionary.datasets WHERE status='active'").bind(json!([{"glosses":["aircraft landing facility"],"hypernyms":[{"word":"transport facility"}],"translations":[{"code":"zh","word":"机场"}]}])).execute(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM dictionary.choice_semantics WHERE normalized_term='airport'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    let p = plan(&f).await;
    let run = open(&f, &p).await;
    let qs = questions(&f, &run).await;
    let names: Vec<_> = qs["items"][0]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["spelling"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"airport"));
    assert_eq!(names.len(), 4);
    sqlx::query(
        "DELETE FROM dictionary.entry_contents WHERE normalized_term IN ('tomato','alternate3')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.0, 200);
    assert_eq!(p.1["can_create"], true);
    assert_spelling_fallback(&f).await;
}
#[sqlx::test]
async fn hidden_dictionary_equivalence_and_other_senses_block_second_answers(pool: PgPool) {
    let f = fixture(&pool).await;
    // Candidate's displayed sense is orange; another sense explicitly defines it as apple.
    sqlx::query("UPDATE dictionary.entry_contents SET senses=senses || $1::jsonb WHERE normalized_term='orange'").bind(json!([{"id":"hidden","glosses":["Synonym of apple."],"translations":[{"code":"zh","word":"另一义项"}]}])).execute(&pool).await.unwrap();
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.0, 200, "{}", p.1);
    assert_eq!(p.1["can_create"], true);
    assert_spelling_fallback(&f).await;
}
#[sqlx::test]
async fn inactive_dictionary_and_projection_overflow_fail_closed(pool: PgPool) {
    let f = fixture(&pool).await;
    sqlx::query("UPDATE dictionary.datasets SET status='retired' WHERE status='active'")
        .execute(&pool)
        .await
        .unwrap();
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.1["can_create"], true);
    assert_spelling_fallback(&f).await;
    sqlx::query("UPDATE dictionary.datasets SET status='active' WHERE version='choice-fixture'")
        .execute(&pool)
        .await
        .unwrap();
    let excessive: Vec<_> = (0..257)
        .map(|_| json!({"glosses":["unresolved secondary sense"]}))
        .collect();
    sqlx::query("UPDATE dictionary.entry_contents SET senses=$1 WHERE normalized_term='tomato'")
        .bind(json!(excessive))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM dictionary.choice_semantics WHERE normalized_term='tomato'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.1["can_create"], true);
    assert_spelling_fallback(&f).await;
}
#[sqlx::test]
async fn semantic_projection_roundtrip_and_dictionary_updates_are_consistent(pool: PgPool) {
    let f = fixture(&pool).await;
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM dictionary.choice_semantics")
            .fetch_one(&pool)
            .await
            .unwrap()
            > 0
    );
    sqlx::raw_sql(include_str!(
        "../migrations/20261030040000_choice_semantics.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261030040000_choice_semantics.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.1["can_create"], true);
    sqlx::query("UPDATE dictionary.entry_contents SET senses='[]' WHERE normalized_term='tomato'")
        .execute(&pool)
        .await
        .unwrap();
    let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
    assert_eq!(p.1["can_create"], true);
    assert_spelling_fallback(&f).await;
}

#[sqlx::test]
async fn malformed_equivalence_evidence_is_not_silently_treated_as_no_synonyms(pool: PgPool) {
    let f = fixture(&pool).await;
    let original: Value = sqlx::query_scalar(
        "SELECT senses FROM dictionary.entry_contents WHERE normalized_term='tomato'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    for (field, malformed) in [
        ("synonyms", json!([{"unknown":"apple"}])),
        ("synonyms", json!([""])),
        ("translations", json!([{"code":"zh","word":null}])),
        ("translations", json!([{"word":"苹果"}])),
    ] {
        let mut senses = original.clone();
        senses[0][field] = malformed;
        sqlx::query(
            "UPDATE dictionary.entry_contents SET senses=$1 WHERE normalized_term='tomato'",
        )
        .bind(senses)
        .execute(&pool)
        .await
        .unwrap();
        let p = call(&f, "POST", "/api/v1/me/learning-plans/preview", preview(&f)).await;
        assert_eq!(p.0, 200, "{field}: {}", p.1);
        assert_eq!(p.1["can_create"], true, "{field}");
        assert_spelling_fallback(&f).await;
    }
}
#[sqlx::test]
async fn dictionary_import_before_publication_is_indexed_when_word_becomes_active(pool: PgPool) {
    let f = fixture(&pool).await;
    sqlx::query("INSERT INTO dictionary.entry_contents(dataset_id,source_key,normalized_term,pos,senses,source_locator) SELECT id,'peach','peach','noun',$1,'owned-fixture' FROM dictionary.datasets WHERE status='active'").bind(json!([{"glosses":["pink stone summer fruit"],"hypernyms":[{"word":"fruit"}],"translations":[{"code":"zh","word":"桃子"}]}])).execute(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM dictionary.choice_semantics WHERE normalized_term='peach'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    let _ = wordlists_support::full_entry(&pool, "peach").await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM dictionary.choice_semantics WHERE normalized_term='peach'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    let _ = f;
}

#[sqlx::test]
async fn adjective_pos_and_explicit_antonym_evidence_are_used_without_spelling_similarity(
    pool: PgPool,
) {
    let mut f = fixture(&pool).await;
    let dataset: i64 =
        sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status='active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut ids = Vec::new();
    for (word, zh, gloss) in [
        ("happy", "高兴的", "feeling pleasure and delight"),
        ("sad", "悲伤的", "feeling sorrow and unhappiness"),
        ("calm", "平静的", "without strong agitation"),
        ("angry", "愤怒的", "feeling strong displeasure"),
    ] {
        let id = wordlists_support::full_entry(&pool, word).await;
        let mut value:Value=sqlx::query_scalar("SELECT p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(id).fetch_one(&pool).await.unwrap();
        value["forms"]["pos"][0]["pos"] = json!("adjective");
        for side in ["uk", "us"] {
            value["forms"]["pos"][0]["forms"][1]["regional_variants"][side]["spelling"] =
                json!(word);
        }
        value["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] = json!(zh);
        sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(id).bind(value).execute(&pool).await.unwrap();
        let senses = json!([{"glosses":[gloss],"hypernyms":[{"word":"emotion"}],"translations":[{"code":"zh","word":zh}],"antonyms":if word=="happy"{json!([{"word":"sad"}])}else{json!([])}}]);
        sqlx::query("INSERT INTO dictionary.entry_contents(dataset_id,source_key,normalized_term,pos,senses,source_locator) VALUES($1,$2,$2,'adj',$3,'owned-fixture')").bind(dataset).bind(word).bind(senses).execute(&pool).await.unwrap();
        ids.push(id);
    }
    let list = account_deletion_support::call(
        &f.state,
        &f.auth,
        "POST",
        "/api/v1/me/wordlists",
        wordlists_support::create_body(&ids[..1]),
    )
    .await;
    assert_eq!(list.0, 200);
    f.list = Uuid::parse_str(list.1["id"].as_str().unwrap()).unwrap();
    let p = plan(&f).await;
    let run = open(&f, &p).await;
    let snapshot: Value = sqlx::query_scalar(
        "SELECT answer_snapshot FROM learning_questions ORDER BY position LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        snapshot["choice"]["content"]["options"][1]["candidate"]["spelling"],
        "sad"
    );
    let qs = questions(&f, &run).await;
    let mut words: Vec<_> = qs["items"][0]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["spelling"].as_str().unwrap())
        .collect();
    words.sort();
    assert_eq!(words, vec!["angry", "calm", "happy", "sad"]);
}

async fn mastery_plan(f: &Fixture, kind: &str, choice: bool, count: i32) -> Value {
    let mut body = preview(f);
    body["practice_mode"] = json!("mastery_v1");
    body["question_type"] = json!(if choice {
        "choice_zh_to_en"
    } else {
        "spelling_zh_to_en"
    });
    body["task_type"] = json!(kind);
    body["daily_question_count"] = if kind == "daily" {
        json!(count)
    } else {
        Value::Null
    };
    let p = call(f, "POST", "/api/v1/me/learning-plans/preview", body.clone()).await;
    assert_eq!(p.0, 200, "{}", p.1);
    assert_eq!(p.1["can_create"], true, "{}", p.1);
    body["name"] = json!("mastery-owned-fixture");
    body["preview_fingerprint"] = p.1["preview_fingerprint"].clone();
    body["idempotency_key"] = json!(Uuid::now_v7());
    let p = call(f, "POST", "/api/v1/me/learning-plans", body).await;
    assert_eq!(p.0, 200, "{}", p.1);
    p.1
}
async fn fixture_list(f: &mut Fixture, ids: &[Uuid]) {
    let list = account_deletion_support::call(
        &f.state,
        &f.auth,
        "POST",
        "/api/v1/me/wordlists",
        wordlists_support::create_body(ids),
    )
    .await;
    assert_eq!(list.0, 200, "{}", list.1);
    f.list = Uuid::parse_str(list.1["id"].as_str().unwrap()).unwrap();
}
#[sqlx::test]
async fn mastery_bounds_new_words_total_attempts_and_spaced_retries(pool: PgPool) {
    let mut f = fixture(&pool).await;
    let ids = seed_corpus(&pool, f.entries[0], 40, 500, 20000, "mastery", "备用").await;
    fixture_list(&mut f, &ids).await;
    let p = mastery_plan(&f, "longterm", false, 20).await;
    let r = open(&f, &p).await;
    assert_eq!(r["target_count"], 20);
    assert_eq!(r["practice_mode"], "mastery_v1");
    let run = r["id"].as_str().unwrap();
    let mut prompts = Vec::new();
    for n in 0..30 {
        let page = call(
            &f,
            "GET",
            &format!("/api/v1/me/learning-runs/{run}/questions?page_size=50"),
            Value::Null,
        )
        .await;
        let next = page.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["answered"] == false)
            .unwrap();
        prompts.push(next["prompt"].clone());
        let answer =
            json!({"idempotency_key":Uuid::now_v7(),"question_id":next["id"],"answer":"不确定"});
        let receipt = call(
            &f,
            "POST",
            &format!("/api/v1/me/learning-runs/{run}/answers"),
            answer.clone(),
        )
        .await;
        assert_eq!(receipt.0, 200, "{n}: {}", receipt.1);
        assert_eq!(receipt.1["run"]["answered_count"], n + 1);
        assert!(receipt.1["run"]["target_count"].as_i64().unwrap() <= 30);
        let replay = call(
            &f,
            "POST",
            &format!("/api/v1/me/learning-runs/{run}/answers"),
            answer,
        )
        .await;
        assert_eq!(replay.1["answer_id"], receipt.1["answer_id"]);
    }
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT count(*),count(*) FILTER(WHERE NOT is_repeat),count(*) FILTER(WHERE is_repeat) FROM learning_answers WHERE run_id=$1").bind(Uuid::parse_str(run).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (30, 20, 10));
    let gaps:Vec<i32>=sqlx::query_scalar("SELECT q.position-p.position FROM learning_questions q JOIN learning_questions p ON p.id=q.retry_of WHERE q.run_id=$1").bind(Uuid::parse_str(run).unwrap()).fetch_all(&pool).await.unwrap();
    assert!(gaps.iter().all(|gap| *gap >= 4));
    let fresh = open(&f, &p).await;
    assert_ne!(fresh["id"], r["id"]);
    assert_eq!(fresh["target_count"], 20);
    let overlap:i64=sqlx::query_scalar("SELECT count(*) FROM learning_questions q WHERE q.run_id=$1 AND EXISTS(SELECT 1 FROM learning_questions old WHERE old.run_id=$2 AND old.unit_key=q.unit_key)").bind(Uuid::parse_str(fresh["id"].as_str().unwrap()).unwrap()).bind(Uuid::parse_str(run).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(overlap, 0);
}
#[sqlx::test]
async fn mastery_choice_confirmation_is_cross_day_and_snapshots_restore(pool: PgPool) {
    let mut f = fixture(&pool).await;
    let ids = f.entries.clone();
    fixture_list(&mut f, &ids).await;
    let p = mastery_plan(&f, "longterm", true, 20).await;
    let r = open(&f, &p).await;
    let run = r["id"].as_str().unwrap();
    for _ in 0..4 {
        let qs = questions(&f, &r).await;
        let q = qs["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["answered"] == false)
            .unwrap();
        let correct:String=sqlx::query_scalar("SELECT answer_snapshot->'choice'->>'correct_option_id' FROM learning_questions WHERE id=$1").bind(Uuid::parse_str(q["id"].as_str().unwrap()).unwrap()).fetch_one(&pool).await.unwrap();
        let a = call(
            &f,
            "POST",
            &format!("/api/v1/me/learning-runs/{run}/answers"),
            json!({"idempotency_key":Uuid::now_v7(),"question_id":q["id"],"answer":correct}),
        )
        .await;
        assert_eq!(a.0, 200, "{}", a.1);
    }
    let state = call(
        &f,
        "GET",
        &format!(
            "/api/v1/me/learning-plans/{}",
            p["task"]["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(state.1["mastery"]["mastered_count"], 0);
    assert_eq!(state.1["mastery"]["learning_count"], 4);
    assert_eq!(state.1["current_cycle"]["covered_count"], 0);
    assert_eq!(state.1["current_cycle"]["remaining_count"], 4);
    let cycles = call(
        &f,
        "GET",
        &format!(
            "/api/v1/me/learning-plans/{}/cycles",
            p["task"]["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(cycles.1["items"][0]["covered_count"], 0);
    assert_eq!(state.1["status"], "review_waiting");
    // Move only owned fixture scheduling evidence to the previous business day.
    sqlx::query("UPDATE learning_cycle_units SET practice_last_success_day=practice_last_success_day-1,practice_last_day=practice_last_day-1,practice_next_day=practice_next_day-1 WHERE cycle_id=$1").bind(Uuid::parse_str(p["current_cycle"]["id"].as_str().unwrap()).unwrap()).execute(&pool).await.unwrap();
    let review = open(&f, &p).await;
    assert_ne!(review["id"], r["id"]);
    let before = questions(&f, &review).await;
    assert_eq!(before, questions(&f, &review).await);
    assert!(
        before["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["practice_kind"] == "review")
    );
    let rr = review["id"].as_str().unwrap();
    for _ in 0..4 {
        let qs = questions(&f, &review).await;
        let q = qs["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["answered"] == false)
            .unwrap();
        let correct:String=sqlx::query_scalar("SELECT answer_snapshot->'choice'->>'correct_option_id' FROM learning_questions WHERE id=$1").bind(Uuid::parse_str(q["id"].as_str().unwrap()).unwrap()).fetch_one(&pool).await.unwrap();
        let a = call(
            &f,
            "POST",
            &format!("/api/v1/me/learning-runs/{rr}/answers"),
            json!({"idempotency_key":Uuid::now_v7(),"question_id":q["id"],"answer":correct}),
        )
        .await;
        assert_eq!(a.0, 200, "{}", a.1);
    }
    let state = call(
        &f,
        "GET",
        &format!(
            "/api/v1/me/learning-plans/{}",
            p["task"]["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(state.1["mastery"]["mastered_count"], 4);
    let interval:Option<i32>=sqlx::query_scalar("SELECT min(practice_next_day-practice_last_success_day) FROM learning_cycle_units WHERE cycle_id=$1").bind(Uuid::parse_str(p["current_cycle"]["id"].as_str().unwrap()).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(interval, Some(3));
    let err = sqlx::raw_sql(include_str!(
        "../migrations/20261030050000_mastery_practice.down.sql"
    ))
    .execute(&pool)
    .await;
    assert!(err.is_err());
}

#[sqlx::test]
async fn mastery_retry_order_idempotency_and_hidden_answer_contract(pool: PgPool) {
    let mut f = fixture(&pool).await;
    let ids = f.entries.clone();
    fixture_list(&mut f, &ids).await;
    let p = mastery_plan(&f, "longterm", true, 20).await;
    let r = open(&f, &p).await;
    let path = format!(
        "/api/v1/me/learning-runs/{}/answers",
        r["id"].as_str().unwrap()
    );
    let before = questions(&f, &r).await;
    let first = &before["items"][0];
    let out_of_order=call(&f,"POST",&path,json!({"idempotency_key":Uuid::now_v7(),"question_id":before["items"][1]["id"],"answer":"不确定"})).await;
    assert_eq!(out_of_order.0, 409);
    let body =
        json!({"idempotency_key":Uuid::now_v7(),"question_id":first["id"],"answer":"不确定"});
    let (a, b) = tokio::join!(
        call(&f, "POST", &path, body.clone()),
        call(&f, "POST", &path, body)
    );
    assert_eq!(a.0, 200, "{}", a.1);
    assert_eq!(b.0, 200, "{}", b.1);
    assert_eq!(a.1["answer_id"], b.1["answer_id"]);
    assert_eq!(a.1["run"]["target_count"], 5);
    let after = questions(&f, &r).await;
    let retry = &after["items"][4];
    assert_eq!(retry["practice_kind"], "retry");
    assert_eq!(retry["prompt"], first["prompt"]);
    assert!(retry["feedback"].is_null());
    assert_ne!(retry["options"], first["options"]);
    assert_eq!(after, questions(&f, &r).await);
    // Archiving a pending source must not permit shortening the required gap.
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=$1")
        .bind(f.entries[1])
        .execute(&pool)
        .await
        .unwrap();
    let attempt=call(&f,"POST",&path,json!({"idempotency_key":Uuid::now_v7(),"question_id":after["items"][1]["id"],"answer":"不确定"})).await;
    assert_eq!(attempt.0, 409);
}
#[sqlx::test]
async fn mastery_small_batches_do_not_repeat_immediately_and_old_clients_are_gated(pool: PgPool) {
    let f = fixture(&pool).await;
    sqlx::raw_sql(include_str!(
        "../migrations/20261030050000_mastery_practice.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261030050000_mastery_practice.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let p = mastery_plan(&f, "longterm", false, 20).await;
    let old_list = account_deletion_support::call(
        &f.state,
        &f.auth,
        "GET",
        "/api/v1/me/learning-plans",
        Value::Null,
    )
    .await;
    assert_eq!(old_list.0, 200);
    assert_eq!(old_list.1["items"], json!([]));
    let old_detail = account_deletion_support::call(
        &f.state,
        &f.auth,
        "GET",
        &format!(
            "/api/v1/me/learning-plans/{}",
            p["task"]["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(old_detail.0, 409);
    let old_cap = account_deletion_support::call(
        &f.state,
        &f.auth,
        "GET",
        "/api/v1/me/learning-capabilities",
        Value::Null,
    )
    .await;
    assert!(old_cap.1.get("practice_modes").is_none());
    let r = open(&f, &p).await;
    let qs = questions(&f, &r).await;
    let a=call(&f,"POST",&format!("/api/v1/me/learning-runs/{}/answers",r["id"].as_str().unwrap()),json!({"idempotency_key":Uuid::now_v7(),"question_id":qs["items"][0]["id"],"answer":"不确定"})).await;
    assert_eq!(a.0, 200, "{}", a.1);
    assert_eq!(a.1["run"]["state"], "completed");
    assert_eq!(a.1["run"]["target_count"], 1);
    assert_eq!(
        questions(&f, &r).await["items"].as_array().unwrap().len(),
        1
    );
}

#[sqlx::test]
async fn mastery_daily_caps_the_initial_queue_and_recovers_the_completed_day(pool: PgPool) {
    let mut f = fixture(&pool).await;
    let ids = f.entries.clone();
    fixture_list(&mut f, &ids).await;
    let mut invalid = preview(&f);
    invalid["practice_mode"] = json!("mastery_v1");
    invalid["daily_question_count"] = json!(21);
    assert_eq!(
        call(&f, "POST", "/api/v1/me/learning-plans/preview", invalid)
            .await
            .0,
        400
    );
    let p = mastery_plan(&f, "daily", true, 20).await;
    let r = open(&f, &p).await;
    assert_eq!(r["target_count"], 4);
    let run = r["id"].as_str().unwrap();
    for _ in 0..4 {
        let qs = questions(&f, &r).await;
        let q = qs["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["answered"] == false)
            .unwrap();
        let correct:String=sqlx::query_scalar("SELECT answer_snapshot->'choice'->>'correct_option_id' FROM learning_questions WHERE id=$1").bind(Uuid::parse_str(q["id"].as_str().unwrap()).unwrap()).fetch_one(&pool).await.unwrap();
        let a = call(
            &f,
            "POST",
            &format!("/api/v1/me/learning-runs/{run}/answers"),
            json!({"idempotency_key":Uuid::now_v7(),"question_id":q["id"],"answer":correct}),
        )
        .await;
        assert_eq!(a.0, 200, "{}", a.1);
    }
    let restored = open(&f, &p).await;
    assert_eq!(restored["id"], r["id"]);
    assert_eq!(restored["state"], "completed");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_completions WHERE run_id=$1")
            .bind(Uuid::parse_str(run).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let state = call(
        &f,
        "GET",
        &format!(
            "/api/v1/me/learning-plans/{}",
            p["task"]["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(state.1["can_start"], false);
    assert_eq!(state.1["status"], "today_completed");
    assert_eq!(state.1["mastery"]["mastered_count"], 0);
}

#[sqlx::test]
async fn mastery_invalidated_source_does_not_recover_an_unusable_active_run(pool: PgPool) {
    let mut f = fixture(&pool).await;
    let ids = seed_corpus(&pool, f.entries[0], 40, 800, 40000, "reopen", "备用").await;
    fixture_list(&mut f, &ids).await;
    let p = mastery_plan(&f, "longterm", false, 20).await;
    let first = open(&f, &p).await;
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=$1")
        .bind(ids[0])
        .execute(&pool)
        .await
        .unwrap();
    let view = call(
        &f,
        "GET",
        &format!("/api/v1/me/learning-runs/{}", first["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(view.1["state"], "invalidated");
    let next = open(&f, &p).await;
    assert_ne!(next["id"], first["id"]);
    assert_eq!(next["state"], "active");
    assert_eq!(next["target_count"], 20);
    let old: String = sqlx::query_scalar("SELECT state FROM learning_runs WHERE id=$1")
        .bind(Uuid::parse_str(first["id"].as_str().unwrap()).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(old, "invalidated");
}

async fn republish_same_fixture(pool: &PgPool, entry: Uuid) {
    let publication = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) SELECT $2,entry_id,publication_number+1,source_revision+1,content_schema_version,snapshot,$3,published_by_admin_id FROM lexicon.entry_publications WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(entry).bind(publication).bind(publication.as_bytes().to_vec()).execute(pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=$2 WHERE id=$1")
        .bind(entry)
        .bind(publication)
        .execute(pool)
        .await
        .unwrap();
}
#[sqlx::test]
async fn mastery_refresh_sources_rebinds_republished_choices_without_losing_progress(pool: PgPool) {
    let mut f = fixture(&pool).await;
    let ids = f.entries.clone();
    fixture_list(&mut f, &ids).await;
    let p = mastery_plan(&f, "longterm", true, 20).await;
    let r = open(&f, &p).await;
    let run = r["id"].as_str().unwrap();
    let qs = questions(&f, &r).await;
    let q = &qs["items"][0];
    let answer:String=sqlx::query_scalar("SELECT answer_snapshot->'choice'->>'correct_option_id' FROM learning_questions WHERE id=$1").bind(Uuid::parse_str(q["id"].as_str().unwrap()).unwrap()).fetch_one(&pool).await.unwrap();
    let accepted = call(
        &f,
        "POST",
        &format!("/api/v1/me/learning-runs/{run}/answers"),
        json!({"idempotency_key":Uuid::now_v7(),"question_id":q["id"],"answer":answer}),
    )
    .await;
    assert_eq!(accepted.0, 200, "{}", accepted.1);
    republish_same_fixture(&pool, f.entries[1]).await;
    let state = call(
        &f,
        "GET",
        &format!(
            "/api/v1/me/learning-plans/{}",
            p["task"]["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(state.1["status"], "source_blocked");
    let body = json!({"operation":"refresh_sources","after_cycle_id":p["current_cycle"]["id"],"expected_revision":p["task"]["revision"]});
    let refreshed = call(
        &f,
        "POST",
        &format!(
            "/api/v1/me/learning-plans/{}/cycles/preview",
            p["task"]["id"].as_str().unwrap()
        ),
        body,
    )
    .await;
    assert_eq!(
        refreshed.0, 200,
        "a restored publication must have a progress-preserving refresh path: {}",
        refreshed.1
    );
    assert_eq!(
        refreshed.1["preview"]["can_create"], true,
        "{}",
        refreshed.1
    );
    assert_eq!(refreshed.1["source_refresh"]["changed_content_count"], 0);
    let request = refresh_confirmation(&p, &refreshed.1, Uuid::now_v7());
    sqlx::raw_sql("CREATE FUNCTION fail_source_refresh() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.operation='refresh_sources' THEN RAISE EXCEPTION 'owned fixture rollback probe'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_source_refresh BEFORE INSERT ON learning_cycle_requests FOR EACH ROW EXECUTE FUNCTION fail_source_refresh();").execute(&pool).await.unwrap();
    let failed = call(&f, "POST", &cycle_path(&p), request.clone()).await;
    assert_eq!(failed.0, 500);
    let old_run: String = sqlx::query_scalar("SELECT state FROM learning_runs WHERE id=$1")
        .bind(Uuid::parse_str(run).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(old_run, "active");
    sqlx::raw_sql("DROP TRIGGER fail_source_refresh ON learning_cycle_requests; DROP FUNCTION fail_source_refresh();").execute(&pool).await.unwrap();
    let fixed = call(&f, "POST", &cycle_path(&p), request.clone()).await;
    assert_eq!(fixed.0, 200, "{}", fixed.1);
    assert_eq!(fixed.1["id"], p["current_cycle"]["id"]);
    let preserved:(Uuid,i16)=sqlx::query_as("SELECT covered_answer_id,practice_success_days FROM learning_cycle_units WHERE id=(SELECT cycle_unit_id FROM learning_answers WHERE id=$1)").bind(Uuid::parse_str(accepted.1["answer_id"].as_str().unwrap()).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(
        preserved,
        (
            Uuid::parse_str(accepted.1["answer_id"].as_str().unwrap()).unwrap(),
            1
        )
    );
    let next = open(&f, &p).await;
    assert_eq!(next["target_count"], 3);
    assert_eq!(questions(&f, &next).await, questions(&f, &next).await);
    let replay = call(&f, "POST", &cycle_path(&p), request).await;
    assert_eq!(replay.0, 200);
    let still_active = call(
        &f,
        "GET",
        &format!("/api/v1/me/learning-runs/{}", next["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(still_active.1["state"], "active");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_cycles")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261030060000_mastery_source_refresh.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
}

fn cycle_path(p: &Value) -> String {
    format!(
        "/api/v1/me/learning-plans/{}/cycles",
        p["task"]["id"].as_str().unwrap()
    )
}
fn refresh_request(p: &Value) -> Value {
    json!({"operation":"refresh_sources","after_cycle_id":p["current_cycle"]["id"],"expected_revision":p["task"]["revision"]})
}
fn refresh_confirmation(p: &Value, preview: &Value, key: Uuid) -> Value {
    let mut body = refresh_request(p);
    body["preview_fingerprint"] = preview["preview"]["preview_fingerprint"].clone();
    body["idempotency_key"] = json!(key);
    body
}
async fn preview_source_refresh(f: &Fixture, p: &Value) -> Value {
    let r = call(
        f,
        "POST",
        &format!("{}/preview", cycle_path(p)),
        refresh_request(p),
    )
    .await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
async fn extra_fruit_fixture(pool: &PgPool) {
    let id = wordlists_support::full_entry(pool, "peach").await;
    let mut value:Value=sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(id).fetch_one(pool).await.unwrap();
    for side in ["uk", "us"] {
        value["forms"]["pos"][0]["forms"][1]["regional_variants"][side]["spelling"] =
            json!("peach");
    }
    value["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] = json!("桃子");
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(id).bind(value).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO dictionary.entry_contents(dataset_id,source_key,normalized_term,pos,senses,source_locator) SELECT id,'peach','peach','noun',$1,'owned-fixture' FROM dictionary.datasets WHERE status='active'").bind(json!([{"glosses":["pink stone summer fruit"],"hypernyms":[{"word":"fruit"}],"translations":[{"code":"zh","word":"桃子"}]}])).execute(pool).await.unwrap();
}
#[sqlx::test]
async fn mastery_refresh_sources_restores_current_public_membership_but_never_renews_revoked_access(
    pool: PgPool,
) {
    sqlx::raw_sql(include_str!(
        "../migrations/20261030060000_mastery_source_refresh.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261030060000_mastery_source_refresh.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let mut f = fixture(&pool).await;
    let ids = f.entries.clone();
    fixture_list(&mut f, &ids).await;
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(f.list)
        .execute(&pool)
        .await
        .unwrap();
    let (_, borrower) = account_deletion_support::setup_bound(&pool).await;
    sqlx::query(
        "INSERT INTO student_profiles(user_id,cefr_level,english_variant) VALUES($1,'A1','BrE')",
    )
    .bind(borrower.subject)
    .execute(&pool)
    .await
    .unwrap();
    f.auth = borrower;
    let p = mastery_plan(&f, "longterm", true, 20).await;
    let r = open(&f, &p).await;
    sqlx::query("UPDATE wordlists SET state='draft' WHERE id=$1")
        .bind(f.list)
        .execute(&pool)
        .await
        .unwrap();
    let denied = preview_source_refresh(&f, &p).await;
    assert_eq!(denied["preview"]["can_create"], false);
    assert_eq!(denied["source_refresh"]["refreshed_count"], 0);
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(f.list)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE wordlist_items SET learning_membership_id=gen_random_uuid() WHERE wordlist_id=$1 AND entry_id=$2").bind(f.list).bind(f.entries[0]).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET wordlist_archive_generation=wordlist_archive_generation+1 WHERE id=$1")
        .bind(f.entries[0])
        .execute(&pool)
        .await
        .unwrap();
    extra_fruit_fixture(&pool).await;
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=$1")
        .bind(f.entries[3])
        .execute(&pool)
        .await
        .unwrap();
    let refreshed = preview_source_refresh(&f, &p).await;
    assert_eq!(refreshed["source_refresh"]["refreshed_count"], 3);
    assert_eq!(refreshed["source_refresh"]["unavailable_count"], 1);
    let fixed = call(
        &f,
        "POST",
        &cycle_path(&p),
        refresh_confirmation(&p, &refreshed, Uuid::now_v7()),
    )
    .await;
    assert_eq!(fixed.0, 200, "{}", fixed.1);
    assert_eq!(fixed.1["total_count"], 4);
    assert_eq!(fixed.1["blocked_count"], 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_cycle_units WHERE cycle_id=$1")
            .bind(Uuid::parse_str(p["current_cycle"]["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap(),
        4
    );
    let old = call(
        &f,
        "GET",
        &format!("/api/v1/me/learning-runs/{}", r["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(old.1["state"], "cancelled");
}
#[sqlx::test]
async fn mastery_refresh_sources_resets_only_changed_content_and_rejects_stale_confirmation(
    pool: PgPool,
) {
    let mut f = fixture(&pool).await;
    let ids = f.entries.clone();
    fixture_list(&mut f, &ids).await;
    let p = mastery_plan(&f, "longterm", false, 20).await;
    let r = open(&f, &p).await;
    let qs = questions(&f, &r).await;
    let q = &qs["items"][0];
    assert_eq!(q["prompt"]["definition"], "苹果");
    let path = format!(
        "/api/v1/me/learning-runs/{}/answers",
        r["id"].as_str().unwrap()
    );
    let first = call(
        &f,
        "POST",
        &path,
        json!({"idempotency_key":Uuid::now_v7(),"question_id":q["id"],"answer":"apple"}),
    )
    .await;
    assert_eq!(first.0, 200, "{}", first.1);
    sqlx::query("UPDATE learning_cycle_units SET practice_success_days=2,practice_interval_step=3,practice_next_day=practice_next_day+13 WHERE covered_answer_id=$1").bind(Uuid::parse_str(first.1["answer_id"].as_str().unwrap()).unwrap()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=jsonb_set(snapshot,'{meanings,pos,0,senses,0,definitions,0,content,text}',to_jsonb('苹果的新释义'::text)) WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(f.entries[0]).execute(&pool).await.unwrap();
    let before = preview_source_refresh(&f, &p).await;
    assert_eq!(before["source_refresh"]["changed_content_count"], 1);
    let another = &qs["items"][1];
    let another_answer: String = sqlx::query_scalar(
        "SELECT answer_snapshot->'answers'->>0 FROM learning_questions WHERE id=$1",
    )
    .bind(Uuid::parse_str(another["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    let answered=call(&f,"POST",&path,json!({"idempotency_key":Uuid::now_v7(),"question_id":another["id"],"answer":another_answer})).await;
    assert_eq!(answered.0, 200, "{}", answered.1);
    let stale = call(
        &f,
        "POST",
        &cycle_path(&p),
        refresh_confirmation(&p, &before, Uuid::now_v7()),
    )
    .await;
    assert_eq!(stale.0, 409);
    let fresh = preview_source_refresh(&f, &p).await;
    let fixed = call(
        &f,
        "POST",
        &cycle_path(&p),
        refresh_confirmation(&p, &fresh, Uuid::now_v7()),
    )
    .await;
    assert_eq!(fixed.0, 200, "{}", fixed.1);
    let retained:(i16,i16,Uuid)=sqlx::query_as("SELECT practice_success_days,practice_interval_step,covered_answer_id FROM learning_cycle_units WHERE covered_answer_id=$1").bind(Uuid::parse_str(first.1["answer_id"].as_str().unwrap()).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(
        retained,
        (
            0,
            0,
            Uuid::parse_str(first.1["answer_id"].as_str().unwrap()).unwrap()
        )
    );
    let unchanged: i16 = sqlx::query_scalar(
        "SELECT practice_success_days FROM learning_cycle_units WHERE covered_answer_id=$1",
    )
    .bind(Uuid::parse_str(answered.1["answer_id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unchanged, 1);
    let next = open(&f, &p).await;
    let page = questions(&f, &next).await;
    assert_eq!(page["items"][0]["prompt"]["definition"], "苹果的新释义");
    let old_prompt: String = sqlx::query_scalar(
        "SELECT prompt_snapshot->>'definition' FROM learning_questions WHERE id=$1",
    )
    .bind(Uuid::parse_str(q["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(old_prompt, "苹果");
}

#[sqlx::test]
async fn mastery_custom_targets_remain_distinct_and_keep_retry_identity(pool: PgPool) {
    let f = fixture(&pool).await;
    sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1")
        .bind(f.list)
        .execute(&pool)
        .await
        .unwrap();
    for n in 0..6 {
        sqlx::query("INSERT INTO wordlist_items(wordlist_id,item_id,custom_word,position) VALUES($1,$2,$3,$4)")
            .bind(f.list).bind(Uuid::now_v7()).bind(json!({"word":format!("custom-{n}"),"definition":format!("自定义{n}")})).bind(n).execute(&pool).await.unwrap();
    }
    let p = mastery_plan(&f, "longterm", true, 20).await;
    let run = open(&f, &p).await;
    assert_eq!(run["target_count"], 6);
    let before = questions(&f, &run).await;
    assert!(
        before["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["content_available"] == true && q.get("options").is_none())
    );
    let result = call(
        &f,
        "POST",
        &learning_tasks_support::answer_path(&run),
        learning_tasks_support::answer_body(&before["items"][0], "不确定"),
    )
    .await;
    assert_eq!(result.0, 200, "{}", result.1);
    assert_eq!(result.1["run"]["target_count"], 7);
    let after = questions(&f, &run).await;
    let retry = after["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["practice_kind"] == "retry")
        .unwrap();
    assert_eq!(retry["content_available"], true);
    assert_eq!(retry["prompt"], before["items"][0]["prompt"]);
    let copied:bool=sqlx::query_scalar("SELECT retry.custom_item_id=original.custom_item_id AND retry.source_membership_id=original.source_membership_id AND retry.entry_id IS NULL FROM learning_questions retry JOIN learning_questions original ON original.id=retry.retry_of WHERE retry.id=$1")
        .bind(Uuid::parse_str(retry["id"].as_str().unwrap()).unwrap()).fetch_one(&pool).await.unwrap();
    assert!(copied);
    assert_eq!(questions(&f, &run).await, after);
}

#[sqlx::test]
async fn mastery_refresh_sources_preserves_distinct_custom_targets_and_old_snapshots(pool: PgPool) {
    let f = fixture(&pool).await;
    sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1")
        .bind(f.list)
        .execute(&pool)
        .await
        .unwrap();
    let items: Vec<_> = (0..3)
        .map(|n| {
            (
                Uuid::now_v7(),
                json!({"word":format!("custom-{n}"),"definition":format!("自定义{n}")}),
            )
        })
        .collect();
    for (position, (item, word)) in items.iter().enumerate() {
        sqlx::query("INSERT INTO wordlist_items(wordlist_id,item_id,custom_word,position) VALUES($1,$2,$3,$4)")
            .bind(f.list).bind(item).bind(word).bind(position as i32).execute(&pool).await.unwrap();
    }
    let p = mastery_plan(&f, "longterm", true, 20).await;
    let run = open(&f, &p).await;
    let snapshot: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(q) ORDER BY position) FROM learning_questions q WHERE run_id=$1",
    )
    .bind(Uuid::parse_str(run["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1")
        .bind(f.list)
        .execute(&pool)
        .await
        .unwrap();
    for (position, (item, word)) in items.iter().enumerate() {
        sqlx::query("INSERT INTO wordlist_items(wordlist_id,item_id,custom_word,position) VALUES($1,$2,$3,$4)")
            .bind(f.list).bind(item).bind(word).bind(position as i32).execute(&pool).await.unwrap();
    }
    let preview = preview_source_refresh(&f, &p).await;
    assert_eq!(preview["source_refresh"]["refreshed_count"], 3);
    assert_eq!(preview["source_refresh"]["changed_content_count"], 0);
    let result = call(
        &f,
        "POST",
        &cycle_path(&p),
        refresh_confirmation(&p, &preview, Uuid::now_v7()),
    )
    .await;
    assert_eq!(result.0, 200, "{}", result.1);
    let preserved: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(q) ORDER BY position) FROM learning_questions q WHERE run_id=$1",
    )
    .bind(Uuid::parse_str(run["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(preserved, snapshot);
    let next = open(&f, &p).await;
    assert_ne!(next["id"], run["id"]);
    assert_eq!(next["target_count"], 3);
    assert!(
        questions(&f, &next).await["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["content_available"] == true)
    );
}

#[sqlx::test]
async fn refreshing_legacy_mastery_choices_to_spelling_updates_capability_guard(pool: PgPool) {
    let f = fixture(&pool).await;
    let p = mastery_plan(&f, "longterm", true, 20).await;
    sqlx::query("UPDATE learning_cycles SET generation_version='choice_algorithm_v1',grading_version='choice_id_v1' WHERE task_id=$1")
        .bind(Uuid::parse_str(p["task"]["id"].as_str().unwrap()).unwrap()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=ANY($1)")
        .bind(&f.entries[1..])
        .execute(&pool)
        .await
        .unwrap();
    let preview = preview_source_refresh(&f, &p).await;
    assert_eq!(preview["source_refresh"]["refreshed_count"], 1);
    let result = call(
        &f,
        "POST",
        &cycle_path(&p),
        refresh_confirmation(&p, &preview, Uuid::now_v7()),
    )
    .await;
    assert_eq!(result.0, 200, "{}", result.1);
    let path = format!(
        "/api/v1/me/learning-plans/{}",
        p["task"]["id"].as_str().unwrap()
    );
    assert_eq!(
        call_with_capabilities(&f, "GET", &path, Value::Null, "choice-v1,mastery-v1")
            .await
            .0,
        409
    );
    let run = open(&f, &p).await;
    let q = questions(&f, &run).await;
    assert!(q["items"][0].get("options").is_none());
    let answer = call(
        &f,
        "POST",
        &learning_tasks_support::answer_path(&run),
        learning_tasks_support::answer_body(&q["items"][0], "apple"),
    )
    .await;
    assert_eq!(answer.0, 200, "{}", answer.1);
    assert_eq!(answer.1["is_correct"], true);
}
