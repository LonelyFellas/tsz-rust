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
    sqlx::raw_sql(include_str!(
        "../migrations/20261028010000_vocabulary_mcq.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261028010000_vocabulary_mcq.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
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
