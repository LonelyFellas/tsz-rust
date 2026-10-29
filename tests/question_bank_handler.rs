mod account_deletion_support;
mod learning_tasks_support;
mod wordlists_support;
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    routing::post,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;
use tsz_rust::{
    auth::extract::AuthUser,
    question_bank::worker::{AgentConfig, tick},
    state::AppState,
};
use uuid::Uuid;
const BANK: &str = "/api/v1/admin/question-bank";
const CALLBACK: &str = "callback-test-service-token-at-least-32-bytes";
async fn request(
    state: &AppState,
    token: &str,
    choice: bool,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");
    if choice {
        builder = builder.header("X-TSZ-Learning-Capabilities", "choice-v1");
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn learner(
    state: &AppState,
    auth: &AuthUser,
    method: &str,
    path: &str,
    body: Value,
) -> Value {
    let token = state
        .token_manager
        .generate_with_version(auth.subject, "student", auth.security_version)
        .unwrap();
    let r = request(state, &token, true, method, path, body).await;
    assert_eq!(r.0, 200, "{path}: {}", r.1);
    r.1
}
#[derive(Clone)]
struct MockAgent {
    question: Value,
    reviews: Arc<AtomicUsize>,
    run: Uuid,
}
async fn mock_run(State(mock): State<MockAgent>, Json(input): Json<Value>) -> Json<Value> {
    Json(
        json!({"contract_version":1,"id":mock.run,"external_request_id":input["external_request_id"],"capability":"vocabulary_mcq_zh_en","state":"succeeded","error_code":null,"result":{"outcome":"draft","question":mock.question}}),
    )
}
async fn mock_review(State(mock): State<MockAgent>, Json(body): Json<Value>) -> Json<Value> {
    assert!(body["event_id"].is_string());
    mock.reviews.fetch_add(1, Ordering::SeqCst);
    Json(json!({"applied":true}))
}
struct Fixture {
    state: AppState,
    auth: AuthUser,
    list: Uuid,
    admin: Uuid,
    candidates: Vec<Value>,
    reviews: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture(pool: &PgPool) -> Fixture {
    let (mut state, auth, list) = learning_tasks_support::setup(pool).await;
    state.learning_memory_enabled = true;
    sqlx::query("INSERT INTO learning_memory_enrollments(user_id,enabled) VALUES($1,true)")
        .bind(auth.subject)
        .execute(pool)
        .await
        .unwrap();
    let target: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM wordlist_items WHERE wordlist_id=$1")
            .bind(list)
            .fetch_one(pool)
            .await
            .unwrap();
    let mut ids = vec![target];
    for word in ["banana", "orange", "tomato"] {
        ids.push(wordlists_support::full_entry(pool, word).await);
    }
    for (i, id) in ids.iter().enumerate() {
        let mut value:Value=sqlx::query_scalar("SELECT p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id WHERE e.id=$1").bind(id).fetch_one(pool).await.unwrap();
        let primary = value["forms"]["pos"][0]["forms"][0]["regional_variants"]["us"]["spelling"]
            .as_str()
            .unwrap()
            .to_owned();
        value["forms"]["pos"][0]["forms"][0]["regional_variants"]["us"]["spelling"] =
            json!(format!(" {primary}-us "));
        for side in ["uk", "us"] {
            value["forms"]["pos"][0]["forms"][1]["regional_variants"][side]["spelling"] =
                json!(format!("alternate{i}"));
        }
        value["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] =
            json!(["苹果", "香蕉", "橙子", "番茄"][i]);
        sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(id).bind(value).execute(pool).await.unwrap();
    }
    let admin = wordlists_support::admin(pool).await;
    let mut candidates = vec![];
    for id in ids {
        let r = wordlists_support::admin_call(
            &state,
            admin,
            "GET",
            &format!("{BANK}/candidates?entry_id={id}&question_level=A1&answer_profile=BrE"),
            Value::Null,
        )
        .await;
        assert_eq!(r.0, 200, "{}", r.1);
        candidates.push(r.1["items"][0].clone());
    }
    let options: Vec<_> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| json!({"option_id":(["A","B","C","D"][i]),"candidate":c}))
        .collect();
    let question = json!({"target":candidates[0],"prompt":"苹果","question_level":"A1","answer_profile":"BrE","options":options,"correct_option_id":"A","requires_human_review":true,"proposal":{"distractors":candidates[1..].iter().map(|c|json!({"candidate_ref":c["candidate_ref"],"why_plausible":"同属常见食物","why_wrong":format!("{}不是苹果",c["definition"].as_str().unwrap())})).collect::<Vec<_>>(),"concerns":[]},"semantic_review":{"verdict":"no_obvious_issue","issues":[]}});
    let reviews = Arc::new(AtomicUsize::new(0));
    let mock = MockAgent {
        question,
        reviews: reviews.clone(),
        run: Uuid::now_v7(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/runs", post(mock_run))
        .route("/v1/review-events", post(mock_review))
        .route(
            "/v1/runs/{id}/cancel",
            post(|| async { Json(json!({"state":"cancelled"})) }),
        )
        .with_state(mock);
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    state.vocabulary_agent = Some(Arc::new(AgentConfig {
        base_url: format!("http://{addr}"),
        outbound_token: "outbound-test-service-token-at-least-32-bytes".into(),
        inbound_token: CALLBACK.into(),
        client: reqwest::Client::new(),
    }));
    Fixture {
        state,
        auth,
        list,
        admin,
        candidates,
        reviews,
        server,
    }
}
fn generation_input(f: &Fixture) -> Value {
    json!({"idempotency_key":Uuid::now_v7(),"target":{"candidate_ref":f.candidates[0]["candidate_ref"],"source":f.candidates[0]["source"],"answer_profile":"BrE"},"question_level":"A1","answer_profile":"BrE"})
}
async fn generate(f: &Fixture) -> Value {
    assert!(
        f.candidates[0]["accepted_spellings"]
            .as_array()
            .unwrap()
            .contains(&json!("apple-us"))
    );
    let body = generation_input(f);
    let r = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "POST",
        &format!("{BANK}/generations"),
        body.clone(),
    )
    .await;
    assert_eq!(r.0, 200, "{}", r.1);
    let retry = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "POST",
        &format!("{BANK}/generations"),
        body,
    )
    .await;
    assert_eq!(retry.1["id"], r.1["id"]);
    tick(&f.state).await.unwrap();
    let generation = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "GET",
        &format!("{BANK}/generations/{}", r.1["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(generation.1["state"], "succeeded", "{}", generation.1);
    let question = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "GET",
        &format!(
            "{BANK}/questions/{}",
            generation.1["question_id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(question.0, 200);
    assert_eq!(question.1["state"], "draft");
    question.1
}
async fn approve(f: &Fixture, q: &Value) -> Value {
    let r=wordlists_support::admin_call(&f.state,f.admin,"POST",&format!("{BANK}/questions/{}/review",q["id"].as_str().unwrap()),json!({"expected_revision":q["revision"],"decision":"approve","reason_codes":[],"comment":"已核对词义唯一性"})).await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
async fn plan(f: &Fixture, choice: bool) -> Value {
    let mut input = json!({"task_type":"daily","wordlist_ids":[f.list],"daily_question_count":1,"schedule":null});
    if choice {
        input["question_type"] = json!("choice_zh_to_en");
    }
    let preview = learner(
        &f.state,
        &f.auth,
        "POST",
        "/api/v1/me/learning-plans/preview",
        input.clone(),
    )
    .await;
    assert_eq!(preview["can_create"], true, "{preview}");
    input["preview_fingerprint"] = preview["preview_fingerprint"].clone();
    input["name"] = json!("隔离题型验收");
    input["idempotency_key"] = json!(Uuid::now_v7());
    learner(
        &f.state,
        &f.auth,
        "POST",
        "/api/v1/me/learning-plans",
        input,
    )
    .await
}
async fn open(f: &Fixture, p: &Value) -> Value {
    learner(
        &f.state,
        &f.auth,
        "POST",
        &format!(
            "/api/v1/me/learning-plans/{}/runs",
            p["task"]["id"].as_str().unwrap()
        ),
        learning_tasks_support::start_body(None),
    )
    .await
}
async fn questions(f: &Fixture, r: &Value) -> Value {
    learner(
        &f.state,
        &f.auth,
        "GET",
        &format!(
            "/api/v1/me/learning-runs/{}/questions",
            r["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await
}

#[sqlx::test]
async fn reviewed_choice_resumes_without_answer_leaks_and_shares_reward_deduplication(
    pool: PgPool,
) {
    sqlx::raw_sql("CREATE SCHEMA reward_clock; CREATE TABLE reward_clock.instant(at timestamptz NOT NULL); INSERT INTO reward_clock.instant VALUES('2026-10-17T06:00:00Z'); CREATE FUNCTION reward_clock.clock_timestamp() RETURNS timestamptz LANGUAGE sql VOLATILE AS 'SELECT at FROM reward_clock.instant';").execute(&pool).await.unwrap();
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
    let f = fixture(&timed).await;
    sqlx::query("INSERT INTO learning_reward_policies(id,rule_version,effective_business_day,enabled,daily_amount,minimum_units) VALUES(gen_random_uuid(),'mcq-test','2026-10-18',true,5,1)")
        .execute(&timed)
        .await
        .unwrap();
    sqlx::query("UPDATE reward_clock.instant SET at='2026-10-18T06:00:00Z'")
        .execute(&pool)
        .await
        .unwrap();
    let empty=learner(&f.state,&f.auth,"POST","/api/v1/me/learning-plans/preview",json!({"question_type":"choice_zh_to_en","task_type":"daily","wordlist_ids":[f.list],"daily_question_count":1,"schedule":null})).await;
    assert_eq!(empty["can_create"], false);
    let draft = generate(&f).await;
    let approved = approve(&f, &draft).await;
    assert_eq!(approved["revision"], 2);
    tick(&f.state).await.unwrap();
    assert_eq!(f.reviews.load(Ordering::SeqCst), 1);
    let p = plan(&f, true).await;
    assert_eq!(p["task"]["question_type"], "choice_zh_to_en");
    let r = open(&f, &p).await;
    assert_eq!(r["question_type"], "choice_zh_to_en");
    let qs = questions(&f, &r).await;
    let q = &qs["items"][0];
    assert_eq!(q["options"].as_array().unwrap().len(), 4);
    assert_eq!(q["feedback"], Value::Null);
    let text = serde_json::to_string(q).unwrap();
    for forbidden in [
        "correct_option_id",
        "explanation",
        "candidate_ref",
        "entry_id",
        "source",
        "苹果：",
    ] {
        assert!(!text.contains(forbidden), "{text}");
    }
    let restored = questions(&f, &open(&f, &p).await).await;
    assert_eq!(restored, qs);
    let old = account_deletion_support::call(
        &f.state,
        &f.auth,
        "GET",
        "/api/v1/me/learning-plans",
        Value::Null,
    )
    .await;
    assert_eq!(old.0, 200);
    assert!(old.1["items"].as_array().unwrap().is_empty());
    let old_detail = account_deletion_support::call(
        &f.state,
        &f.auth,
        "GET",
        &format!("/api/v1/me/learning-runs/{}", r["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(old_detail.0, 409);
    let token = f
        .state
        .token_manager
        .generate_with_version(f.auth.subject, "student", 0)
        .unwrap();
    let path = learning_tasks_support::answer_path(&r);
    let bad = request(
        &f.state,
        &token,
        true,
        "POST",
        &path,
        learning_tasks_support::answer_body(q, "apple"),
    )
    .await;
    assert_eq!(bad.0, 400);
    let option = q["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["spelling"] == "apple")
        .unwrap()["option_id"]
        .as_str()
        .unwrap();
    let body = learning_tasks_support::answer_body(q, option);
    let answer = learner(&f.state, &f.auth, "POST", &path, body.clone()).await;
    assert_eq!(answer["is_correct"], true);
    assert_eq!(answer["question"]["feedback"]["correct_option_id"], option);
    assert!(answer["question"]["feedback"]["explanation"].is_string());
    let retry = learner(&f.state, &f.auth, "POST", &path, body).await;
    assert_eq!(retry["answer_id"], answer["answer_id"]);
    let memories: i64 =
        sqlx::query_scalar("SELECT count(*) FROM learning_memory_events WHERE user_id=$1")
            .bind(f.auth.subject)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(memories, 0);
    let spelling = plan(&f, false).await;
    assert!(spelling["task"].get("question_type").is_none());
    let sr = open(&f, &spelling).await;
    let sq = questions(&f, &sr).await;
    assert!(sq["items"][0].get("options").is_none());
    learner(
        &f.state,
        &f.auth,
        "POST",
        &learning_tasks_support::answer_path(&sr),
        learning_tasks_support::answer_body(&sq["items"][0], "apple"),
    )
    .await;
    let reward = learner(
        &f.state,
        &f.auth,
        "GET",
        "/api/v1/me/coins/learning-rewards",
        Value::Null,
    )
    .await;
    assert_eq!(reward["qualifying_units"], 1);
    assert_eq!(reward["awarded_amount"], "5");
    let settlements: i64 =
        sqlx::query_scalar("SELECT count(*) FROM learning_reward_settlements WHERE user_id=$1")
            .bind(f.auth.subject)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(settlements, 1);
    let answers: i64 = sqlx::query_scalar("SELECT count(*) FROM learning_answers WHERE run_id=$1")
        .bind(Uuid::parse_str(r["id"].as_str().unwrap()).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(answers, 1);
    let changed=wordlists_support::admin_call(&f.state,f.admin,"POST",&format!("{BANK}/questions/{}/review",draft["id"].as_str().unwrap()),json!({"expected_revision":1,"decision":"reject","reason_codes":["other"],"comment":"stale"})).await;
    assert_eq!(changed.0, 409);
}

#[sqlx::test]
async fn delegation_auth_cancellation_and_all_four_sources_fail_closed(pool: PgPool) {
    let f = fixture(&pool).await;
    let body = generation_input(&f);
    let created = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "POST",
        &format!("{BANK}/generations"),
        body,
    )
    .await;
    assert_eq!(created.0, 200);
    let id = created.1["id"].as_str().unwrap();
    let path = format!("/api/internal/agent/v1/requests/{id}/context");
    assert_eq!(
        request(&f.state, "wrong", false, "GET", &path, Value::Null)
            .await
            .0,
        401
    );
    let context = request(&f.state, CALLBACK, false, "GET", &path, Value::Null).await;
    assert_eq!(context.0, 200, "{}", context.1);
    assert_eq!(context.1["target"], f.candidates[0]);
    let mut rebound = f.candidates[1]["source"].clone();
    rebound["publication_id"] = f.candidates[0]["source"]["publication_id"].clone();
    let bad=request(&f.state,CALLBACK,false,"POST",&format!("/api/internal/agent/v1/requests/{id}/entries/batch-get"),json!({"contract_version":1,"items":[{"candidate_ref":f.candidates[1]["candidate_ref"],"source":rebound,"answer_profile":"BrE"}]})).await;
    assert_eq!(bad.0, 409);
    sqlx::query("UPDATE admins SET permission_version=permission_version+1 WHERE id=$1")
        .bind(f.admin)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&f.state, CALLBACK, false, "GET", &path, Value::Null)
            .await
            .0,
        403
    );
    sqlx::query("UPDATE question_generations SET submitted_at=now() WHERE id=$1")
        .bind(Uuid::parse_str(id).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let cancel = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "POST",
        &format!("{BANK}/generations/{id}/cancel"),
        json!({"expected_revision":1}),
    )
    .await;
    assert_eq!(cancel.0, 200);
    tick(&f.state).await.unwrap();
    let recovered:bool=sqlx::query_scalar("SELECT cancel_delivered AND agent_run_id IS NOT NULL FROM question_generations WHERE id=$1").bind(Uuid::parse_str(id).unwrap()).fetch_one(&pool).await.unwrap();
    assert!(recovered);
    assert_eq!(
        request(&f.state, CALLBACK, false, "GET", &path, Value::Null)
            .await
            .0,
        403
    );
    let draft = generate(&f).await;
    let option_id: Uuid =
        serde_json::from_value(f.candidates[3]["source"]["entry_id"].clone()).unwrap();
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=$1")
        .bind(option_id)
        .execute(&pool)
        .await
        .unwrap();
    let result = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "POST",
        &format!("{BANK}/questions/{}/review", draft["id"].as_str().unwrap()),
        json!({"expected_revision":1,"decision":"approve","reason_codes":[],"comment":""}),
    )
    .await;
    assert_eq!(result.0, 409);
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=true WHERE id=$1")
        .bind(option_id)
        .execute(&pool)
        .await
        .unwrap();
    let approved = approve(&f, &draft).await;
    let p = plan(&f, true).await;
    let run = open(&f, &p).await;
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=$1")
        .bind(option_id)
        .execute(&pool)
        .await
        .unwrap();
    let qs = questions(&f, &run).await;
    assert_eq!(qs["items"][0]["content_available"], false);
    assert!(qs["items"][0].get("options").is_none());
    let disabled=wordlists_support::admin_call(&f.state,f.admin,"POST",&format!("{BANK}/questions/{}/review",draft["id"].as_str().unwrap()),json!({"expected_revision":approved["revision"],"decision":"disable","reason_codes":[],"comment":"来源停用"})).await;
    assert_eq!(disabled.0, 200);
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=true WHERE id=$1")
        .bind(option_id)
        .execute(&pool)
        .await
        .unwrap();
    let edited=wordlists_support::admin_call(&f.state,f.admin,"PATCH",&format!("{BANK}/questions/{}",draft["id"].as_str().unwrap()),json!({"expected_revision":disabled.1["revision"],"prompt":"一种常见水果，中文名称为苹果","options":f.candidates.iter().map(|c|json!({"candidate_ref":c["candidate_ref"],"source":c["source"],"answer_profile":"BrE"})).collect::<Vec<_>>(),"option_explanations":["苹果的英文","香蕉不是苹果","橙子不是苹果","番茄不是苹果"],"explanation":"核对四个来源后修订"})).await;
    assert_eq!(edited.0, 200, "{}", edited.1);
    assert_eq!(edited.1["state"], "draft");
    approve(&f, &edited.1).await;
}

#[sqlx::test]
async fn vocabulary_migration_down_up_and_no_silent_loss_of_learning(pool: PgPool) {
    let migrator = sqlx::migrate!("./migrations");
    migrator.undo(&pool, 20261028000000).await.unwrap();
    migrator.run(&pool).await.unwrap();
    let f = fixture(&pool).await;
    let draft = generate(&f).await;
    approve(&f, &draft).await;
    plan(&f, true).await;
    let mut tx = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261028010000_vocabulary_mcq.down.sql"
        ))
        .execute(&mut *tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM learning_tasks WHERE question_type='choice_zh_to_en'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, 1);
}

#[sqlx::test]
async fn candidate_references_do_not_rebind_after_distractor_republication(pool: PgPool) {
    let f = fixture(&pool).await;
    let created = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "POST",
        &format!("{BANK}/generations"),
        generation_input(&f),
    )
    .await;
    assert_eq!(created.0, 200);
    let id = created.1["id"].as_str().unwrap();
    let old = &f.candidates[1];
    let entry: Uuid = serde_json::from_value(old["source"]["entry_id"].clone()).unwrap();
    let publication = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) SELECT $2,entry_id,3,3,content_schema_version,snapshot,$3,published_by_admin_id FROM lexicon.entry_publications WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)").bind(entry).bind(publication).bind(publication.as_bytes().to_vec()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=$2 WHERE id=$1")
        .bind(entry)
        .bind(publication)
        .execute(&pool)
        .await
        .unwrap();
    let fresh = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "GET",
        &format!("{BANK}/candidates?entry_id={entry}&question_level=A1&answer_profile=BrE"),
        Value::Null,
    )
    .await;
    assert_eq!(fresh.0, 200);
    assert_ne!(fresh.1["items"][0]["candidate_ref"], old["candidate_ref"]);
    assert_eq!(
        fresh.1["items"][0]["source"]["form_id"],
        old["source"]["form_id"]
    );
    let context = request(
        &f.state,
        CALLBACK,
        false,
        "GET",
        &format!("/api/internal/agent/v1/requests/{id}/context"),
        Value::Null,
    )
    .await;
    assert_eq!(context.0, 200);
    let stale=request(&f.state,CALLBACK,false,"POST",&format!("/api/internal/agent/v1/requests/{id}/entries/batch-get"),json!({"contract_version":1,"items":[{"candidate_ref":old["candidate_ref"],"source":old["source"],"answer_profile":"BrE"}]})).await;
    assert_eq!(stale.0, 409);
}

#[sqlx::test]
async fn legacy_single_resource_routes_reject_choice_coverage_plans(pool: PgPool) {
    let f = fixture(&pool).await;
    let draft = generate(&f).await;
    approve(&f, &draft).await;
    let p = plan(&f, true).await;
    let id = p["task"]["id"].as_str().unwrap();
    let token = f
        .state
        .token_manager
        .generate_with_version(f.auth.subject, "student", f.auth.security_version)
        .unwrap();
    let old_path = format!("/api/v1/me/learning-tasks/{id}/runs");
    let rejected = request(
        &f.state,
        &token,
        true,
        "POST",
        &old_path,
        learning_tasks_support::start_body(None),
    )
    .await;
    assert_eq!(rejected.0, StatusCode::CONFLICT, "{}", rejected.1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM learning_runs WHERE task_id=$1")
        .bind(Uuid::parse_str(id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    open(&f, &p).await;
    assert_eq!(
        request(
            &f.state,
            &token,
            true,
            "POST",
            &old_path,
            learning_tasks_support::start_body(None)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test]
async fn edited_rejection_delivers_the_reviewed_question_revision(pool: PgPool) {
    let f = fixture(&pool).await;
    let draft = generate(&f).await;
    let id = draft["id"].as_str().unwrap();
    let reordered = [
        &f.candidates[0],
        &f.candidates[2],
        &f.candidates[1],
        &f.candidates[3],
    ];
    let edited = wordlists_support::admin_call(&f.state, f.admin, "PATCH", &format!("{BANK}/questions/{id}"), json!({
        "expected_revision": draft["revision"], "prompt":"一种水果：苹果",
        "options":reordered.iter().map(|c|json!({"candidate_ref":c["candidate_ref"],"source":c["source"],"answer_profile":"BrE"})).collect::<Vec<_>>(),
        "option_explanations":["正确项","修改后的干扰项一","修改后的干扰项二","修改后的干扰项三"],
        "explanation":"当前修订的解析"
    })).await;
    assert_eq!(edited.0, 200, "{}", edited.1);
    let rejected = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "POST",
        &format!("{BANK}/questions/{id}/review"),
        json!({
            "expected_revision":edited.1["revision"], "decision":"reject",
            "reason_codes":["weak_distractor"], "comment":"本次编辑后的选项仍太简单"
        }),
    )
    .await;
    assert_eq!(rejected.0, 200, "{}", rejected.1);
    let payload: Value =
        sqlx::query_scalar("SELECT payload FROM question_review_deliveries WHERE question_id=$1")
            .bind(Uuid::parse_str(id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    let revision: Uuid =
        sqlx::query_scalar("SELECT question_revision_id FROM question_bank WHERE id=$1")
            .bind(Uuid::parse_str(id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(payload["question_revision_id"], json!(revision));
    assert_eq!(payload["example"]["prompt"], edited.1["content"]["prompt"]);
    assert_eq!(
        payload["example"]["distractors"][0],
        edited.1["content"]["options"][1]["candidate"]
    );
    assert_eq!(payload["example"]["lesson"], "本次编辑后的选项仍太简单");
}

#[sqlx::test]
async fn agent_candidates_reach_eligible_entries_beyond_early_library_limits(pool: PgPool) {
    for i in 0..301 {
        let id = wordlists_support::full_entry(&pool, &format!("advanced{i}")).await;
        let mut value: Value = sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)")
            .bind(id).fetch_one(&pool).await.unwrap();
        for pos in value["meanings"]["pos"].as_array_mut().unwrap() {
            for sense in pos["senses"].as_array_mut().unwrap() {
                for definition in sense["definitions"].as_array_mut().unwrap() {
                    definition["level"] = json!("C2");
                }
            }
        }
        sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)")
            .bind(id).bind(value).execute(&pool).await.unwrap();
    }
    // Many definitions on a single entry must not exhaust the distractor entry budget.
    let crowded = wordlists_support::full_entry(&pool, "polysemous").await;
    let mut value: Value = sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)")
        .bind(crowded).fetch_one(&pool).await.unwrap();
    let template = value["meanings"]["pos"][0]["senses"][0]["definitions"][0].clone();
    value["meanings"]["pos"][0]["senses"][0]["definitions"] = json!(
        (0..100)
            .map(|i| {
                let mut definition = template.clone();
                definition["id"] = json!(Uuid::now_v7());
                definition["content_id"] = json!(Uuid::now_v7());
                definition["content"]["text"] = json!(format!("不同义项{i}"));
                definition
            })
            .collect::<Vec<_>>()
    );
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)")
        .bind(crowded).bind(value).execute(&pool).await.unwrap();
    let f = fixture(&pool).await;
    let generation = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "POST",
        &format!("{BANK}/generations"),
        generation_input(&f),
    )
    .await;
    assert_eq!(generation.0, 200);
    let mut input = json!({"contract_version":1,"target_source":f.candidates[0]["source"],"question_level":"A1","answer_profile":"BrE","exclude_candidate_refs":[],"limit":40});
    let path = format!(
        "/api/internal/agent/v1/requests/{}/candidates/search",
        generation.1["id"].as_str().unwrap()
    );
    let first = request(&f.state, CALLBACK, false, "POST", &path, input.clone()).await;
    assert_eq!(first.0, 200, "{}", first.1);
    let items = first.1["items"].as_array().unwrap();
    for expected in &f.candidates[1..] {
        assert!(
            items
                .iter()
                .any(|item| item["source"]["entry_id"] == expected["source"]["entry_id"]),
            "eligible distractor is unreachable"
        );
    }
    let identities: std::collections::HashSet<_> = items
        .iter()
        .map(|c| c["source"]["entry_id"].as_str().unwrap())
        .collect();
    assert_eq!(identities.len(), items.len());
    assert!(
        items
            .iter()
            .all(|item| item["level"] == "A1" && item["part_of_speech"] == "noun")
    );
    input["exclude_candidate_refs"] = json!(
        items
            .iter()
            .map(|item| item["candidate_ref"].clone())
            .collect::<Vec<_>>()
    );
    let second = request(&f.state, CALLBACK, false, "POST", &path, input).await;
    assert_eq!(second.0, 200);
    assert!(
        second.1["items"].as_array().unwrap().is_empty(),
        "do not recycle other senses/forms from already-issued distractor entries"
    );
}

async fn batch_call(f: &Fixture, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    wordlists_support::admin_call(&f.state, f.admin, method, &format!("{BANK}/{path}"), body).await
}
fn selection() -> Value {
    json!({"question_levels":["B1","A1"],"answer_profiles":["BrE","AmE"]})
}
async fn batch_preview(f: &Fixture, s: Value) -> Value {
    let r = batch_call(f, "POST", "batches/preview", s).await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
async fn create_batch(f: &Fixture, preview: &Value) -> Value {
    let r=batch_call(f,"POST","batches",json!({"idempotency_key":Uuid::now_v7(),"selection":preview["selection"],"preview_token":preview["preview_token"]})).await;
    assert_eq!(r.0, 200, "{}", r.1);
    r.1
}
#[sqlx::test]
async fn candidate_pagination_covers_all_publications_and_excludes_ineligible_sources(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    for i in 0..43 {
        wordlists_support::full_entry(&pool, &format!("batch-word-{i}")).await;
    }
    let excluded = wordlists_support::full_entry(&pool, "batch-excluded").await;
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=$1")
        .bind(excluded)
        .execute(&pool)
        .await
        .unwrap();
    let archived = wordlists_support::full_entry(&pool, "batch-archived").await;
    sqlx::query("UPDATE lexicon.entries SET archived_at=now() WHERE id=$1")
        .bind(archived)
        .execute(&pool)
        .await
        .unwrap();
    let unpublished = wordlists_support::full_entry(&pool, "batch-unpublished").await;
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=NULL WHERE id=$1")
        .bind(unpublished)
        .execute(&pool)
        .await
        .unwrap();
    let mut refs = std::collections::HashSet::new();
    let mut page = 1;
    let total = loop {
        let r = batch_call(
            &f,
            "GET",
            &format!(
                "candidate-page?question_level=C2&answer_profile=BrE&page={page}&page_size=17"
            ),
            Value::Null,
        )
        .await;
        assert_eq!(r.0, 200, "{}", r.1);
        for c in r.1["items"].as_array().unwrap() {
            assert_ne!(c["source"]["entry_id"], json!(excluded));
            assert_ne!(c["source"]["entry_id"], json!(archived));
            assert!(refs.insert(c["candidate_ref"].as_str().unwrap().to_owned()));
        }
        if page == r.1["pagination"]["total_pages"].as_u64().unwrap() {
            break r.1["pagination"]["total"].as_u64().unwrap();
        }
        page += 1;
    };
    assert!(total > 80);
    assert_eq!(refs.len() as u64, total);
    let search = batch_call(
        &f,
        "GET",
        "candidate-page?question_level=C2&answer_profile=BrE&query=batch-word-42",
        Value::Null,
    )
    .await;
    assert_eq!(search.0, 200);
    assert!(!search.1["items"].as_array().unwrap().is_empty());
    let preview = batch_preview(&f, selection()).await;
    // Alternate base forms of one definition must not cause duplicate batch questions.
    assert_eq!(preview["missing"], 47 * 2 * 4);
    let created = create_batch(&f, &preview).await;
    assert_eq!(created["total"], 376); // Crosses the insert chunk boundary without truncation.
}
#[sqlx::test]
async fn batch_idempotency_concurrent_creation_cancellation_and_retry_are_persistent(pool: PgPool) {
    let f = fixture(&pool).await;
    let preview = batch_preview(&f, selection()).await;
    assert_eq!(preview["missing"], 32);
    let body = json!({"idempotency_key":Uuid::now_v7(),"selection":preview["selection"],"preview_token":preview["preview_token"]});
    let (a, b) = tokio::join!(
        batch_call(&f, "POST", "batches", body.clone()),
        batch_call(&f, "POST", "batches", body.clone())
    );
    assert_eq!(a.0, 200, "{}", a.1);
    assert_eq!(b.0, 200, "{}", b.1);
    assert_eq!(a.1["id"], b.1["id"]);
    let mut other = body.clone();
    other["idempotency_key"] = json!(Uuid::now_v7());
    assert_eq!(batch_call(&f, "POST", "batches", other).await.0, 409);
    let now = batch_preview(&f, selection()).await;
    assert_eq!(now["missing"], 0);
    assert!(
        now["coverage"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["generating"] == 8)
    );
    // Single creation shares the batch admission lock and identity, including alternate forms.
    assert_eq!(
        batch_call(&f, "POST", "generations", generation_input(&f))
            .await
            .0,
        409
    );
    let id = a.1["id"].as_str().unwrap();
    let cancelled = batch_call(&f, "POST", &format!("batches/{id}/cancel"), Value::Null).await;
    assert_eq!(cancelled.0, 200, "{}", cancelled.1);
    assert_eq!(cancelled.1["cancelled"], 32);
    let replay = batch_call(&f, "POST", "batches", body).await;
    assert_eq!(replay.1["id"], a.1["id"]);
    assert_eq!(replay.1["state"], "cancelled");
    let detail = batch_call(&f, "GET", &format!("batches/{id}"), Value::Null).await;
    assert_eq!(detail.1["cancelled"], 32);
    let items = batch_call(
        &f,
        "GET",
        &format!("batches/{id}/items?page_size=5&state=cancelled"),
        Value::Null,
    )
    .await;
    assert_eq!(items.1["pagination"]["total"], 32);
    assert_eq!(items.1["items"].as_array().unwrap().len(), 5);
    assert_eq!(batch_preview(&f, selection()).await["missing"], 32);
    let p = batch_preview(&f, selection()).await;
    let input = |key| json!({"idempotency_key":key,"selection":p["selection"],"preview_token":p["preview_token"]});
    let (x, y) = tokio::join!(
        batch_call(&f, "POST", "batches", input(Uuid::now_v7())),
        batch_call(&f, "POST", "batches", input(Uuid::now_v7()))
    );
    assert!(
        (x.0 == 200 && y.0 == 409) || (x.0 == 409 && y.0 == 200),
        "{x:?} {y:?}"
    );

    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM question_generations WHERE batch_id=$1 AND NOT cancel_delivered",
    )
    .bind(Uuid::parse_str(id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 0);
    async fn assert_rollback_blocked(pool: &PgPool) {
        let mut tx = pool.begin().await.unwrap();
        let result = sqlx::raw_sql(include_str!(
            "../migrations/20261028120000_question_generation_batches.down.sql"
        ))
        .execute(&mut *tx)
        .await;
        tx.rollback().await.unwrap();
        let error = result
            .expect_err("rollback must retain active batch scheduling and cancellation ownership");
        assert!(
            error.to_string().contains("batch generation work"),
            "{error}"
        );
        let (tasks, linked): (i64, i64) =
            sqlx::query_as("SELECT count(*),count(batch_id) FROM question_generations")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!((tasks, linked), (64, 64));
        let batches: i64 = sqlx::query_scalar("SELECT count(*) FROM question_generation_batches")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(batches, 2);
    }
    assert_rollback_blocked(&pool).await;
    let active_batch = if x.0 == 200 { &x.1 } else { &y.1 };
    let cancelled = batch_call(
        &f,
        "POST",
        &format!("batches/{}/cancel", active_batch["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(cancelled.0, 200, "{}", cancelled.1);
    let remote: Uuid =
        sqlx::query_scalar("SELECT id FROM question_generations ORDER BY id LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    for state in ["running", "cancelled", "failed"] {
        sqlx::query("UPDATE question_generations SET state=$2,submitted_at=now()-interval '3 hours',expires_at=now()-interval '1 hour',cancel_delivered=false WHERE id=$1").bind(remote).bind(state).execute(&pool).await.unwrap();
        // Expiration alone does not prove that a remote execution stopped.
        assert_rollback_blocked(&pool).await;
    }
    sqlx::query("UPDATE question_generations SET cancel_delivered=true WHERE id=$1")
        .bind(remote)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261028120000_question_generation_batches.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let retained: i64 = sqlx::query_scalar("SELECT count(*) FROM question_generations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(retained, 64);
    sqlx::raw_sql(include_str!(
        "../migrations/20261028120000_question_generation_batches.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
}
#[sqlx::test]
async fn batch_worker_rechecks_sources_and_permissions_and_keeps_queue_alive(pool: PgPool) {
    let f = fixture(&pool).await;
    let s = json!({"question_levels":["A1"],"answer_profiles":["BrE"]});
    let preview = batch_preview(&f, s.clone()).await;
    let batch = create_batch(&f, &preview).await;
    let id = Uuid::parse_str(batch["id"].as_str().unwrap()).unwrap();
    // Simulate a queue that survived a service restart and waited beyond its original TTL.
    sqlx::query(
        "UPDATE question_generations SET expires_at=now()-interval '1 day' WHERE batch_id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    // Pin the known valid target first; the unchanged mock Agent only generates that target.
    sqlx::query("UPDATE question_generations SET next_attempt_at=CASE WHEN target->>'candidate_ref'=$2 THEN now()-interval '1 minute' ELSE now() END WHERE batch_id=$1").bind(id).bind(f.candidates[0]["candidate_ref"].as_str().unwrap()).execute(&pool).await.unwrap();
    tick(&f.state).await.unwrap();
    let rows:Vec<(String,Option<String>)>=sqlx::query_as("SELECT state,error_code FROM question_generations WHERE batch_id=$1 ORDER BY next_attempt_at").bind(id).fetch_all(&pool).await.unwrap();
    assert!(rows.iter().any(|(s, _)| s == "succeeded"), "{rows:?}");
    // Revoking teaching eligibility after enrollment is a terminal task failure.
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=(SELECT (target#>>'{source,entry_id}')::uuid FROM question_generations WHERE batch_id=$1 AND state='queued' ORDER BY id LIMIT 1)").bind(id).execute(&pool).await.unwrap();
    tick(&f.state).await.unwrap();
    let failures:i64=sqlx::query_scalar("SELECT count(*) FROM question_generations WHERE batch_id=$1 AND state='failed' AND error_code='authorization_or_source_changed'").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(failures, 1);
    // Security/permission versions are persisted and rechecked, not inherited from browser state.
    sqlx::query("UPDATE admins SET permission_version=permission_version+1 WHERE id=$1")
        .bind(f.admin)
        .execute(&pool)
        .await
        .unwrap();
    for _ in 0..16 {
        // Advance only the cancellation retry timer. Unconfirmed remote work keeps
        // its execution slot; queued tasks proceed after the acknowledgement.
        sqlx::query("UPDATE question_generations SET next_attempt_at=now() WHERE state IN ('failed','cancelled') AND NOT cancel_delivered").execute(&pool).await.unwrap();
        tick(&f.state).await.unwrap();
    }
    let left:i64=sqlx::query_scalar("SELECT count(*) FROM question_generations WHERE batch_id=$1 AND state IN ('queued','running')").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(left, 0);
    let fresh = batch_preview(&f, s).await;
    assert_eq!(fresh["coverage"][0]["covered"], 0); // Its now-ineligible distractor invalidates coverage.
    assert!(fresh["missing"].as_i64().unwrap() > 0);
    let retry = create_batch(&f, &fresh).await;
    assert_eq!(retry["total"], fresh["missing"]);
}

#[sqlx::test]
async fn worker_limits_admission_and_rotates_batches_while_cancellation_survives_restart(
    pool: PgPool,
) {
    let mut f = fixture(&pool).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel_attempts = Arc::new(AtomicUsize::new(0));
    let app=Router::new().route("/v1/runs",post(|Json(input):Json<Value>|async move {
        Json(json!({"contract_version":1,"id":input["external_request_id"],"external_request_id":input["external_request_id"],"capability":"vocabulary_mcq_zh_en","state":"queued","error_code":null}))
    })).route("/v1/runs/{id}/cancel",post(move || {let attempts=cancel_attempts.clone(); async move {
        if attempts.fetch_add(1,Ordering::SeqCst)==0 {(StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":"retry"})))} else {(StatusCode::OK,Json(json!({"state":"cancelled"})))}
    }}));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    f.state.vocabulary_agent = Some(Arc::new(AgentConfig {
        base_url: format!("http://{addr}"),
        outbound_token: "test-outbound-service-token-at-least-32-bytes".into(),
        inbound_token: CALLBACK.into(),
        client: reqwest::Client::new(),
    }));
    let p = batch_preview(
        &f,
        json!({"question_levels":["A1"],"answer_profiles":["BrE"]}),
    )
    .await;
    let first = create_batch(&f, &p).await;
    let p = batch_preview(
        &f,
        json!({"question_levels":["B1"],"answer_profiles":["BrE"]}),
    )
    .await;
    let second = create_batch(&f, &p).await;
    // Separate worker ticks operate on persistent rows, with no in-memory batch scheduler.
    tick(&f.state).await.unwrap();
    tick(&f.state).await.unwrap();
    tick(&f.state).await.unwrap();
    let admitted:Vec<(Uuid,i64)>=sqlx::query_as("SELECT batch_id,count(*) FROM question_generations WHERE submitted_at IS NOT NULL GROUP BY batch_id").fetch_all(&pool).await.unwrap();
    assert_eq!(admitted.len(), 2);
    assert!(admitted.iter().all(|(_, n)| *n == 1));
    let cancelled = batch_call(
        &f,
        "POST",
        &format!("batches/{}/cancel", first["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(cancelled.0, 200);
    assert!(tick(&f.state).await.is_err());
    tick(&f.state).await.unwrap();
    let admitted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM question_generations WHERE batch_id=$1 AND submitted_at IS NOT NULL",
    )
    .bind(Uuid::parse_str(second["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        admitted, 1,
        "unacknowledged remote cancellation still occupies its execution slot"
    );
    sqlx::query("UPDATE question_generations SET next_attempt_at=now() WHERE state='cancelled' AND NOT cancel_delivered").execute(&pool).await.unwrap();
    tick(&f.state).await.unwrap();
    let delivered: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM question_generations WHERE batch_id=$1 AND cancel_delivered",
    )
    .bind(Uuid::parse_str(first["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(delivered, 8);
    tick(&f.state).await.unwrap();
    let running: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM question_generations WHERE batch_id=$1 AND submitted_at IS NOT NULL",
    )
    .bind(Uuid::parse_str(second["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(running, 2);
    server.abort();
}

#[sqlx::test]
async fn batch_scope_excludes_unowned_entries_and_revoked_edit_permission(pool: PgPool) {
    let f = fixture(&pool).await;
    let own: Uuid = serde_json::from_value(f.candidates[0]["source"]["entry_id"].clone()).unwrap();
    sqlx::query("UPDATE lexicon.entries SET created_by_admin_id=$2 WHERE id=$1")
        .bind(own)
        .bind(f.admin)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(f.admin)
        .execute(&pool)
        .await
        .unwrap();
    for key in ["words.access", "words.edit"] {
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,$2,$1)").bind(f.admin).bind(key).execute(&pool).await.unwrap();
    }
    let token = f
        .state
        .admin_token_manager
        .generate_with_version(f.admin, "admin", 0)
        .unwrap();
    let s = json!({"question_levels":["A1"],"answer_profiles":["BrE"]});
    let preview = request(
        &f.state,
        &token,
        false,
        "POST",
        &format!("{BANK}/batches/preview"),
        s.clone(),
    )
    .await;
    assert_eq!(preview.0, 200, "{}", preview.1);
    assert_eq!(preview.1["missing"], 2);
    let candidates = request(
        &f.state,
        &token,
        false,
        "GET",
        &format!("{BANK}/candidate-page?question_level=A1&answer_profile=BrE"),
        Value::Null,
    )
    .await;
    assert_eq!(candidates.0, 200);
    assert!(
        candidates.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["source"]["entry_id"] == json!(own))
    );
    sqlx::query(
        "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='words.edit'",
    )
    .bind(f.admin)
    .execute(&pool)
    .await
    .unwrap();
    let result=request(&f.state,&token,false,"POST",&format!("{BANK}/batches"),json!({"idempotency_key":Uuid::now_v7(),"selection":s,"preview_token":preview.1["preview_token"]})).await;
    assert_eq!(result.0, 403);
}

#[sqlx::test]
async fn insufficient_candidates_is_terminal_and_can_be_filled_again(pool: PgPool) {
    let mut f = fixture(&pool).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app=Router::new().route("/v1/runs",post(|Json(input):Json<Value>|async move {
        Json(json!({"contract_version":1,"id":input["external_request_id"],"external_request_id":input["external_request_id"],"capability":"vocabulary_mcq_zh_en","state":"succeeded","error_code":null,"result":{"outcome":"insufficient_candidates","reasons":["库内没有足够的不同词条"]}}))
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    f.state.vocabulary_agent = Some(Arc::new(AgentConfig {
        base_url: format!("http://{addr}"),
        outbound_token: "test-outbound-service-token-at-least-32-bytes".into(),
        inbound_token: CALLBACK.into(),
        client: reqwest::Client::new(),
    }));
    let s = json!({"question_levels":["A1"],"answer_profiles":["BrE"]});
    let p = batch_preview(&f, s.clone()).await;
    let first = create_batch(&f, &p).await;
    for _ in 0..8 {
        tick(&f.state).await.unwrap();
    }
    let result = batch_call(
        &f,
        "GET",
        &format!("batches/{}", first["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(result.1["state"], "completed");
    assert_eq!(result.1["insufficient_candidates"], 8);
    let p = batch_preview(&f, s).await;
    assert_eq!(p["missing"], 8);
    let second = create_batch(&f, &p).await;
    assert_ne!(second["id"], first["id"]);
    assert_eq!(second["total"], 8);
    server.abort();
}

#[sqlx::test]
async fn agent_retrieval_ranks_late_related_entries_and_supplements_distinct_words(pool: PgPool) {
    let f = fixture(&pool).await;
    let mut preferred = vec![];
    for (word, definition) in (0..65)
        .map(|i| (format!("zzqx{i}"), "无关释义".to_owned()))
        .chain([
            ("applet".into(), "小程序".into()),
            ("orchard".into(), "苹果园".into()),
            ("apple-synonym".into(), "苹果".into()),
        ])
    {
        let id = wordlists_support::full_entry(&pool, &word).await;
        let mut snapshot: Value = sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)")
            .bind(id).fetch_one(&pool).await.unwrap();
        for side in ["uk", "us"] {
            snapshot["forms"]["pos"][0]["forms"][1]["regional_variants"][side]["spelling"] =
                json!(format!("{word}-alternate"));
        }
        snapshot["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] =
            json!(definition);
        sqlx::query("UPDATE lexicon.entry_publications SET snapshot=$2 WHERE id=(SELECT current_publication_id FROM lexicon.entries WHERE id=$1)")
            .bind(id).bind(snapshot).execute(&pool).await.unwrap();
        if word == "applet" || word == "orchard" {
            preferred.push(id);
        }
    }
    let generation = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "POST",
        &format!("{BANK}/generations"),
        generation_input(&f),
    )
    .await;
    assert_eq!(generation.0, 200);
    let path = format!(
        "/api/internal/agent/v1/requests/{}/candidates/search",
        generation.1["id"].as_str().unwrap()
    );
    let mut input = json!({"contract_version":1,"target_source":f.candidates[0]["source"],"question_level":"A1","answer_profile":"BrE","exclude_candidate_refs":[],"limit":40});
    let first = request(&f.state, CALLBACK, false, "POST", &path, input.clone()).await;
    assert_eq!(first.0, 200, "{}", first.1);
    let items = first.1["items"].as_array().unwrap();
    assert_eq!(items.len(), 40);
    for id in preferred {
        assert!(
            items[..2]
                .iter()
                .any(|c| c["source"]["entry_id"] == json!(id)),
            "related late entry must precede unrelated early entries: target={}, first={:?}",
            f.candidates[0]["spelling"],
            items[..4]
                .iter()
                .map(|c| (&c["spelling"], &c["definition"]))
                .collect::<Vec<_>>()
        );
    }
    assert!(items.iter().all(|c| c["definition"] != "苹果"));
    let repeated = request(&f.state, CALLBACK, false, "POST", &path, input.clone()).await;
    assert_eq!(
        first.1, repeated.1,
        "ranking must be stable for unchanged sources"
    );
    input["exclude_candidate_refs"] = json!(
        items
            .iter()
            .map(|c| &c["candidate_ref"])
            .collect::<Vec<_>>()
    );
    let second = request(&f.state, CALLBACK, false, "POST", &path, input).await;
    assert_eq!(second.0, 200, "{}", second.1);
    let more = second.1["items"].as_array().unwrap();
    assert!(!more.is_empty());
    assert!(more.iter().all(|c| {
        items
            .iter()
            .all(|old| old["source"]["entry_id"] != c["source"]["entry_id"])
    }));
    assert!(more.iter().all(|c| c["definition"] != "苹果"));
}

#[sqlx::test]
async fn workbench_search_is_server_paginated_and_inspects_pinned_publication_sources(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let first = generate(&f).await;
    let second = generate(&f).await;
    let approved = approve(&f, &first).await;
    let r=batch_call(&f,"GET","questions/workbench?query=apple&state=approved&question_level=A1&answer_profile=BrE&page_size=1",Value::Null).await;
    assert_eq!(r.0, 200, "{}", r.1);
    assert_eq!(r.1["pagination"]["total"], 1);
    assert_eq!(r.1["counts"]["approved"], 1);
    assert_eq!(r.1["counts"]["draft"], 1);
    assert_eq!(r.1["items"][0]["id"], approved["id"]);
    assert_eq!(r.1["items"][0]["source_current"], true);
    assert!(r.1["items"][0].get("original_content").is_none());
    let page2 = batch_call(
        &f,
        "GET",
        "questions/workbench?page=2&page_size=1",
        Value::Null,
    )
    .await;
    assert_eq!(page2.0, 200, "{}", page2.1);
    assert_eq!(page2.1["items"].as_array().unwrap().len(), 1);
    assert_eq!(page2.1["pagination"]["total"], 2);
    let source: Uuid =
        serde_json::from_value(f.candidates[1]["source"]["entry_id"].clone()).unwrap();
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=$1")
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    let inspection = batch_call(
        &f,
        "GET",
        &format!("questions/{}/inspection", second["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(inspection.0, 200, "{}", inspection.1);
    assert_eq!(inspection.1["target_current"], true);
    assert_eq!(
        inspection.1["sources"],
        json!([{"option_id":"A","current":true},{"option_id":"B","current":false},{"option_id":"C","current":true},{"option_id":"D","current":true}])
    );
    assert_eq!(inspection.1["question"]["content"], second["content"]);
    let stale = batch_call(&f, "GET", "questions/workbench", Value::Null).await;
    assert_eq!(stale.0, 200);
    assert!(
        stale.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["source_current"] == false)
    );
    let legacy = batch_call(
        &f,
        "GET",
        &format!("questions/{}", second["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(legacy.0, 200);
    assert!(
        legacy.1.get("sources").is_none(),
        "legacy strict response remains unchanged"
    );
    assert_eq!(legacy.1["original_content"], second["original_content"]);
    assert_eq!(
        batch_call(
            &f,
            "GET",
            "questions/workbench?question_level=D1",
            Value::Null
        )
        .await
        .0,
        400
    );
}

fn candidate_search_input() -> Value {
    json!({"question_level":"C2","answer_profile":"BrE","page":1,"page_size":3,"editable_only":true})
}
#[sqlx::test]
async fn candidate_search_paginates_published_bound_senses_and_respects_replacement_scope(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let unpublished = wordlists_support::full_entry(&pool, "unpublished-candidate").await;
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=NULL WHERE id=$1")
        .bind(unpublished)
        .execute(&pool)
        .await
        .unwrap();
    let archived = wordlists_support::full_entry(&pool, "archived-candidate").await;
    sqlx::query("UPDATE lexicon.entries SET archived_at=now() WHERE id=$1")
        .bind(archived)
        .execute(&pool)
        .await
        .unwrap();
    let nonteaching = wordlists_support::full_entry(&pool, "nonteaching-candidate").await;
    sqlx::query("UPDATE lexicon.entries SET teaching_eligible=false WHERE id=$1")
        .bind(nonteaching)
        .execute(&pool)
        .await
        .unwrap();
    let legacy = batch_call(
        &f,
        "GET",
        "candidate-page?question_level=C2&answer_profile=BrE&page_size=100",
        Value::Null,
    )
    .await;
    assert_eq!(legacy.0, 200);
    let expected: std::collections::HashSet<_> = legacy.1["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["candidate_ref"].as_str().unwrap().to_owned())
        .collect();
    let mut actual = std::collections::HashSet::new();
    let mut input = candidate_search_input();
    let mut page = 1;
    loop {
        input["page"] = json!(page);
        let r = batch_call(&f, "POST", "candidate-search", input.clone()).await;
        assert_eq!(r.0, 200, "{}", r.1);
        assert_eq!(r.1["pagination"]["total"], expected.len());
        for c in r.1["items"].as_array().unwrap() {
            assert!(actual.insert(c["candidate_ref"].as_str().unwrap().to_owned()));
        }
        if page >= r.1["pagination"]["total_pages"].as_u64().unwrap() {
            break;
        }
        page += 1;
    }
    assert_eq!(actual, expected);
    let mut replacement = candidate_search_input();
    replacement["editable_only"] = json!(false);
    replacement["page_size"] = json!(100);
    replacement["part_of_speech"] = json!("noun");
    replacement["exclude_entry_ids"] = json!([f.candidates[0]["source"]["entry_id"]]);
    replacement["exclude_spellings"] = f.candidates[0]["accepted_spellings"].clone();
    replacement["excluded_definition"] = f.candidates[0]["definition"].clone();
    replacement["target_spelling"] = f.candidates[0]["spelling"].clone();
    replacement["query"] = json!("香蕉");
    let found = batch_call(&f, "POST", "candidate-search", replacement.clone()).await;
    assert_eq!(found.0, 200, "{}", found.1);
    assert!(!found.1["items"].as_array().unwrap().is_empty());
    assert!(
        found.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["definition"] == "香蕉"
                && c["source"]["entry_id"] != f.candidates[0]["source"]["entry_id"])
    );
    // Filter every accepted spelling, not only the spelling currently shown in an option.
    let mut overlap = replacement.clone();
    overlap["exclude_spellings"] = json!(["ＡＬＴＥＲＮＡＴＥ１"]);
    let excluded_alias = batch_call(&f, "POST", "candidate-search", overlap).await;
    assert_eq!(excluded_alias.0, 200, "{}", excluded_alias.1);
    assert_eq!(excluded_alias.1["pagination"]["total"], 0);
    // Another published meaning in the same entry must also exclude a distractor.
    let mut ambiguous = replacement.clone();
    ambiguous["excluded_definition"] = json!("专用词义");
    let excluded_meaning = batch_call(&f, "POST", "candidate-search", ambiguous).await;
    assert_eq!(excluded_meaning.0, 200);
    assert_eq!(excluded_meaning.1["pagination"]["total"], 0);
    let mut literal = replacement.clone();
    literal["query"] = json!("%_");
    let literal_search = batch_call(&f, "POST", "candidate-search", literal).await;
    assert_eq!(literal_search.0, 200);
    assert_eq!(literal_search.1["pagination"]["total"], 0);
    replacement["part_of_speech"] = json!("verb");
    let wrong_pos = batch_call(&f, "POST", "candidate-search", replacement).await;
    assert_eq!(wrong_pos.0, 200);
    assert_eq!(wrong_pos.1["pagination"]["total"], 0);
    // Generation targets are ownership-scoped, but published distractors remain readable.
    let own: Uuid = serde_json::from_value(f.candidates[0]["source"]["entry_id"].clone()).unwrap();
    sqlx::query("UPDATE lexicon.entries SET created_by_admin_id=$2 WHERE id=$1")
        .bind(own)
        .bind(f.admin)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(f.admin)
        .execute(&pool)
        .await
        .unwrap();
    for key in ["words.access", "words.edit"] {
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,$2,$1)").bind(f.admin).bind(key).execute(&pool).await.unwrap();
    }
    let token = f
        .state
        .admin_token_manager
        .generate_with_version(f.admin, "admin", 0)
        .unwrap();
    input["page"] = json!(1);
    input["page_size"] = json!(100);
    let owned = request(
        &f.state,
        &token,
        false,
        "POST",
        &format!("{BANK}/candidate-search"),
        input.clone(),
    )
    .await;
    assert_eq!(owned.0, 200, "{}", owned.1);
    assert!(
        owned.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["source"]["entry_id"] == json!(own))
    );
    input["editable_only"] = json!(false);
    let shared = request(
        &f.state,
        &token,
        false,
        "POST",
        &format!("{BANK}/candidate-search"),
        input,
    )
    .await;
    assert_eq!(shared.0, 200, "{}", shared.1);
    assert_eq!(shared.1["pagination"]["total"], expected.len());
}

#[sqlx::test]
async fn candidate_index_reactivates_and_roundtrips_without_removing_question_facts(pool: PgPool) {
    let f = fixture(&pool).await;
    let q = generate(&f).await;
    let entry: Uuid =
        serde_json::from_value(f.candidates[0]["source"]["entry_id"].clone()).unwrap();
    let publication: Uuid =
        serde_json::from_value(f.candidates[0]["source"]["publication_id"].clone()).unwrap();
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT lexicon.question_candidate_key_v1(value) FROM unnest($1::text[]) value",
    )
    .bind(vec!["\u{00a0}Ａ’Ｂ—C\u{3000}", "İ"])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(keys, vec!["a'b-c", "i\u{0307}"]);
    sqlx::query("DELETE FROM lexicon.question_candidate_index WHERE publication_id=$1")
        .bind(publication)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=NULL WHERE id=$1")
        .bind(entry)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=$2 WHERE id=$1")
        .bind(entry)
        .bind(publication)
        .execute(&pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.question_candidate_index WHERE publication_id=$1",
    )
    .bind(publication)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        count > 0,
        "reactivating a publication rebuilds its derived candidate rows"
    );
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261029020000_question_candidate_index.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let retained = batch_call(
        &f,
        "GET",
        &format!("questions/{}", q["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(retained.0, 200);
    assert_eq!(retained.1["original_content"], q["original_content"]);
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261029020000_question_candidate_index.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let fk_index: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_indexes WHERE schemaname='lexicon' AND indexname='question_candidate_entry')").fetch_one(&pool).await.unwrap();
    assert!(
        fk_index,
        "entry deletion has a leading FK-support index after rebuild"
    );
    let restored = batch_call(&f, "POST", "candidate-search", candidate_search_input()).await;
    assert_eq!(restored.0, 200, "{}", restored.1);
    assert!(restored.1["pagination"]["total"].as_i64().unwrap() > 0);
    let questions: i64 = sqlx::query_scalar("SELECT count(*) FROM question_bank")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(questions, 1, "only derived search data is rebuilt");
}

#[sqlx::test]
async fn indexed_candidates_resolve_shared_publication_forms_without_changing_references(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let before=batch_call(&f,"POST","candidate-search",json!({"entry_id":f.candidates[0]["source"]["entry_id"],"question_level":"C2","answer_profile":"BrE","page_size":100})).await;
    assert_eq!(before.0, 200);
    let entry: Uuid =
        serde_json::from_value(f.candidates[0]["source"]["entry_id"].clone()).unwrap();
    wordlists_support::share_current_publication(&pool, entry).await;
    let after = batch_call(
        &f,
        "POST",
        "candidate-search",
        json!({"entry_id":entry,"question_level":"C2","answer_profile":"BrE","page_size":100}),
    )
    .await;
    assert_eq!(after.0, 200, "{}", after.1);
    assert_eq!(
        after.1, before.1,
        "shared content slots resolve to the same candidate references and values"
    );
}

#[sqlx::test]
async fn semantic_review_evidence_is_structured_and_unchanged_ambiguity_cannot_be_approved(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let q = generate(&f).await;
    let id = q["id"].as_str().unwrap();
    let inspect_path = format!("{BANK}/questions/{id}/inspection");
    let read =
        wordlists_support::admin_call(&f.state, f.admin, "GET", &inspect_path, Value::Null).await;
    assert_eq!(read.0, 200);
    assert_eq!(read.1["semantic_review"]["verdict"], "no_obvious_issue");
    assert_eq!(read.1["semantic_review_current"], true);
    assert!(q.get("semantic_review").is_none());
    assert!(
        q["content"].get("semantic_review").is_none(),
        "legacy strict responses remain unchanged"
    );
    let issue = json!({"verdict":"needs_revision","issues":["其他选项也可能对应苹果"]});
    sqlx::query("UPDATE question_bank SET semantic_review=$2 WHERE id=$1")
        .bind(Uuid::parse_str(id).unwrap())
        .bind(&issue)
        .execute(&pool)
        .await
        .unwrap();
    let rejected = wordlists_support::admin_call(&f.state, f.admin, "POST", &format!("{BANK}/questions/{id}/review"), json!({"expected_revision":q["revision"],"decision":"approve","reason_codes":[],"comment":""})).await;
    assert_eq!(rejected.0, StatusCode::CONFLICT);
    let options:Vec<_>=q["content"]["options"].as_array().unwrap().iter().map(|o|json!({"candidate_ref":o["candidate"]["candidate_ref"],"source":o["candidate"]["source"],"answer_profile":o["candidate"]["answer_profile"]})).collect();
    let explanations: Vec<_> = q["content"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["explanation"].clone())
        .collect();
    let edit = json!({"expected_revision":q["revision"],"prompt":q["content"]["prompt"],"options":options,"option_explanations":explanations,"explanation":"仅重写解析不能消除原题面歧义"});
    let saved = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "PATCH",
        &format!("{BANK}/questions/{id}"),
        edit.clone(),
    )
    .await;
    assert_eq!(saved.0, 200, "{}", saved.1);
    let rejected = wordlists_support::admin_call(&f.state, f.admin, "POST", &format!("{BANK}/questions/{id}/review"), json!({"expected_revision":saved.1["revision"],"decision":"approve","reason_codes":[],"comment":""})).await;
    assert_eq!(rejected.0, StatusCode::CONFLICT);
    let mut reordered = edit.clone();
    reordered["expected_revision"] = saved.1["revision"].clone();
    reordered["options"].as_array_mut().unwrap().swap(0, 1);
    reordered["option_explanations"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    let reordered = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "PATCH",
        &format!("{BANK}/questions/{id}"),
        reordered,
    )
    .await;
    assert_eq!(reordered.0, 200);
    let rejected = wordlists_support::admin_call(&f.state, f.admin, "POST", &format!("{BANK}/questions/{id}/review"), json!({"expected_revision":reordered.1["revision"],"decision":"approve","reason_codes":[],"comment":""})).await;
    assert_eq!(
        rejected.0,
        StatusCode::CONFLICT,
        "reordering cannot resolve semantic ambiguity"
    );
    let mut changed = edit;
    changed["expected_revision"] = reordered.1["revision"].clone();
    changed["prompt"] = json!("请选择中文“苹果”对应的英文单词");
    let saved = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "PATCH",
        &format!("{BANK}/questions/{id}"),
        changed,
    )
    .await;
    assert_eq!(saved.0, 200, "{}", saved.1);
    let read =
        wordlists_support::admin_call(&f.state, f.admin, "GET", &inspect_path, Value::Null).await;
    assert_eq!(
        read.1["semantic_review"], issue,
        "editing never discards original risk evidence"
    );
    assert_eq!(
        read.1["semantic_review_current"], false,
        "editing is not a new model review"
    );
}

#[sqlx::test]
async fn semantic_review_migration_preserves_legacy_questions_without_inventing_a_verdict(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let q = generate(&f).await;
    let id = q["id"].as_str().unwrap();
    let migrator = sqlx::migrate!("./migrations");
    migrator.undo(&pool, 20261029020000).await.unwrap();
    let preserved: serde_json::Value =
        sqlx::query_scalar("SELECT content FROM question_bank WHERE id=$1")
            .bind(Uuid::parse_str(id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(preserved, q["content"]);
    migrator.run(&pool).await.unwrap();
    let read = wordlists_support::admin_call(
        &f.state,
        f.admin,
        "GET",
        &format!("{BANK}/questions/{id}/inspection"),
        Value::Null,
    )
    .await;
    assert_eq!(read.0, 200);
    assert_eq!(read.1["semantic_review"], Value::Null);
    assert_eq!(read.1["semantic_review_current"], false);
    assert_eq!(read.1["question"]["content"], q["content"]);
}
