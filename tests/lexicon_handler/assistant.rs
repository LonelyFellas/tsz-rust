use super::*;
use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use std::collections::VecDeque;
use tokio::sync::Mutex as AsyncMutex;
use tsz_rust::question_bank::worker::AgentConfig;

#[derive(Clone)]
struct ModelStub {
    replies: Arc<AsyncMutex<VecDeque<Value>>>,
    inputs: Arc<AsyncMutex<Vec<Value>>>,
    revoke: Arc<AsyncMutex<Option<Uuid>>>,
    bump: Arc<AsyncMutex<Option<Uuid>>>,
    header_delay: Arc<std::sync::atomic::AtomicBool>,
    hold: Arc<std::sync::atomic::AtomicBool>,
    gate: Arc<tokio::sync::Notify>,
    pool: PgPool,
}

async fn model_step(
    State(stub): State<ModelStub>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(
        headers["authorization"],
        "Bearer assistant-test-service-token"
    );
    stub.inputs.lock().await.push(body);
    if let Some(id) = stub.revoke.lock().await.take() {
        sqlx::query("UPDATE admins SET status='disabled' WHERE id=$1")
            .bind(id)
            .execute(&stub.pool)
            .await
            .unwrap();
    }
    if let Some(id) = stub.bump.lock().await.take() {
        sqlx::query("UPDATE lexicon.entries SET revision=revision+1 WHERE id=$1")
            .bind(id)
            .execute(&stub.pool)
            .await
            .unwrap();
    }
    Json(
        stub.replies
            .lock()
            .await
            .pop_front()
            .expect("unexpected model retry"),
    )
}

fn reply(content: &str, calls: Value) -> Value {
    json!({"content":content,"tool_calls":calls,"model":"fixture-model","input_tokens":100,"output_tokens":20})
}

async fn model_stream(
    State(stub): State<ModelStub>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let Json(reply) = model_step(State(stub.clone()), headers, Json(body)).await;
    if stub.header_delay.load(std::sync::atomic::Ordering::SeqCst) {
        tokio::time::sleep(std::time::Duration::from_millis(3100)).await;
    }
    let stream =
        futures_util::stream::unfold((stub, reply, 0), |(stub, reply, index)| async move {
            let event = match index {
                0 => json!({"event":"delta","text":reply["content"]}),
                1 => {
                    if stub.hold.load(std::sync::atomic::Ordering::SeqCst) {
                        stub.gate.notified().await;
                    }
                    json!({"event":"complete","step":reply})
                }
                _ => return None,
            };
            Some((
                Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(
                    event.to_string() + "\n",
                )),
                (stub, reply, index + 1),
            ))
        });
    (
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(stream),
    )
}

async fn setup(
    pool: PgPool,
) -> (
    AppState,
    String,
    Uuid,
    ModelStub,
    tokio::task::JoinHandle<()>,
) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = fixture_state(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let actor = seed_admin(&pool).await;
    let bearer = token(&state, actor);
    seed_dictionary_word(&pool, "bank").await;
    let dataset: i64 =
        sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status='active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO dictionary.content_imports(dataset_id,input_sha256,source_locator,record_count,source_version) VALUES($1,repeat('a',64),'assistant-fixture',1,'assistant-fixture-v1')").bind(dataset).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO dictionary.entry_contents(dataset_id,source_key,normalized_term,pos,senses,source_locator) VALUES($1,'assistant-bank','bank','noun',$2,'assistant-fixture')")
        .bind(dataset).bind(json!([{"glosses":["a financial institution"]},{"glosses":["the land beside a river"]}])).execute(&pool).await.unwrap();
    let entry = create_legacy_v3_empty_skeleton(&state, &bearer, "bank").await;
    let stub = ModelStub {
        replies: Arc::new(AsyncMutex::new(VecDeque::new())),
        inputs: Arc::new(AsyncMutex::new(Vec::new())),
        revoke: Arc::new(AsyncMutex::new(None)),
        bump: Arc::new(AsyncMutex::new(None)),
        header_delay: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        hold: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        gate: Arc::new(tokio::sync::Notify::new()),
        pool,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route(
            "/v1/lexicon-assistant/capability",
            axum::routing::get(|| async {
                Json(json!({"available":true,"tool_selection":true,"rules_version":"fixture-v2"}))
            }),
        )
        .route("/v1/lexicon-assistant/step", post(model_step))
        .route("/v1/lexicon-assistant/form-candidates", post(model_step))
        .route("/v1/lexicon-assistant/step/stream", post(model_stream))
        .with_state(stub.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    state.vocabulary_agent = Some(Arc::new(AgentConfig {
        base_url: format!("http://{address}"),
        outbound_token: "assistant-test-service-token".into(),
        client: reqwest::Client::new(),
    }));
    (state, bearer, entry, stub, server)
}

#[sqlx::test]
async fn assistant_stream_delivers_delta_before_upstream_completion(pool: PgPool) {
    let (state, bearer, entry, stub, server) = setup(pool).await;
    stub.hold.store(true, std::sync::atomic::Ordering::SeqCst);
    stub.replies
        .lock()
        .await
        .push_back(reply("先展示这段尚在生成的建议。", json!([])));
    let response = tsz_rust::router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{ROOT}/assistant/stream"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(input(entry).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/x-ndjson"
    );
    let mut stream = response.into_body().into_data_stream();
    let mut received = String::new();
    while !received.contains("先展示这段") {
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        received.push_str(std::str::from_utf8(&bytes).unwrap());
    }
    assert!(!received.contains("\"event\":\"complete\""));
    sqlx::query("UPDATE lexicon.entries SET revision=revision+1 WHERE id=$1")
        .bind(entry)
        .execute(&stub.pool)
        .await
        .unwrap();
    stub.gate.notify_one();
    while let Some(bytes) = stream.next().await {
        received.push_str(std::str::from_utf8(&bytes.unwrap()).unwrap());
    }
    assert!(received.contains("\"event\":\"complete\""));
    assert!(received.contains("\"requires_human_review\":true"));
    assert!(received.contains("回答期间词条已保存过"));
    server.abort();
}

#[sqlx::test]
async fn stream_rechecks_permission_after_waiting_for_headers(pool: PgPool) {
    let (state, bearer, entry, stub, server) = setup(pool).await;
    stub.header_delay
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let actor: Uuid =
        sqlx::query_scalar("SELECT created_by_admin_id FROM lexicon.entries WHERE id=$1")
            .bind(entry)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    *stub.revoke.lock().await = Some(actor);
    stub.replies
        .lock()
        .await
        .push_back(reply("撤权后不得交付的内容", json!([])));
    let response = tsz_rust::router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{ROOT}/assistant/stream"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(input(entry).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let events = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!events.contains("撤权后不得交付的内容"), "{events}");
    assert!(events.contains("\"event\":\"failed\""), "{events}");
    server.abort();
}

fn input(entry: Uuid) -> Value {
    json!({"entry_id":entry,"base_revision":1,"context_id":Uuid::now_v7(),"step":"meanings","local_content":"未保存：我把 bank 填成了金融机构，还缺什么？","context_truncated":false,"question":"查词库和内置词典给我建议","history":[]})
}

#[sqlx::test]
async fn assistant_queries_both_sources_and_rules_without_changing_business_data(pool: PgPool) {
    let (state, bearer, entry, stub, server) = setup(pool.clone()).await;
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(e) FROM lexicon.entries e WHERE id=$1")
        .bind(entry)
        .fetch_one(&pool)
        .await
        .unwrap();
    stub.replies.lock().await.extend([
        reply("",json!([
            {"id":"search","name":"search_lexicon","arguments":"{\"q\":\"bank\"}"},
            {"id":"dictionary","name":"lookup_dictionary","arguments":"{\"q\":\"bank\"}"}
        ])),
        reply("",json!([
            {"id":"read","name":"read_lexicon","arguments":json!({"entry_id":entry}).to_string()},
            {"id":"rules","name":"read_filling_rules","arguments":"{}"}
        ])),
        reply("根据词典和当前未保存内容，建议增加河岸义项。",json!([]))
    ]);
    let (status, result) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/assistant"),
        &bearer,
        None,
        Some(input(entry)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["requires_human_review"], true);
    assert_eq!(result["usage"]["model_calls"], 3);
    assert_eq!(result["usage"]["tool_calls"], 4);
    let sources = result["sources"].as_array().unwrap();
    assert!(
        sources
            .iter()
            .any(|source| source["kind"] == "builtin_dictionary"
                && source["version"] == "assistant-fixture-v1")
    );
    assert!(
        sources
            .iter()
            .any(|source| source["entry_id"] == entry.to_string())
    );
    let inputs = stub.inputs.lock().await;
    let transmitted = inputs.last().unwrap().to_string();
    assert!(transmitted.contains("financial institution"));
    assert!(transmitted.contains("land beside a river"));
    assert!(transmitted.contains("未保存"));
    assert!(!transmitted.contains("词库测试管理员"));
    assert!(!transmitted.contains("assistant-test-service-token"));
    let after: Value = sqlx::query_scalar("SELECT to_jsonb(e) FROM lexicon.entries e WHERE id=$1")
        .bind(entry)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    server.abort();
}

#[sqlx::test]
async fn assistant_rejects_write_tools_and_stale_snapshots_without_retry(pool: PgPool) {
    let (state, bearer, entry, stub, server) = setup(pool).await;
    let mut stale = input(entry);
    stale["base_revision"] = json!(99);
    assert_eq!(
        call(
            &state,
            Method::POST,
            &format!("{ROOT}/assistant"),
            &bearer,
            None,
            Some(stale)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(stub.inputs.lock().await.is_empty());
    stub.replies.lock().await.push_back(reply(
        "",
        json!([{"id":"write","name":"save_lexicon","arguments":"{}"}]),
    ));
    assert_eq!(
        call(
            &state,
            Method::POST,
            &format!("{ROOT}/assistant"),
            &bearer,
            None,
            Some(input(entry))
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(stub.inputs.lock().await.len(), 1);
    stub.replies.lock().await.extend((0..3).map(|index| {
        reply(
            "",
            json!([
                {"id":format!("rules-{index}"),"name":"read_filling_rules","arguments":"{}"}
            ]),
        )
    }));
    assert_eq!(
        call(
            &state,
            Method::POST,
            &format!("{ROOT}/assistant"),
            &bearer,
            None,
            Some(input(entry))
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let inputs = stub.inputs.lock().await;
    assert_eq!(inputs.len(), 4);
    assert_eq!(inputs.last().unwrap()["allow_tools"], false);
    server.abort();
}

#[sqlx::test]
async fn assistant_rechecks_authorization_after_model_wait(pool: PgPool) {
    let (state, bearer, entry, stub, server) = setup(pool.clone()).await;
    let actor: Uuid =
        sqlx::query_scalar("SELECT created_by_admin_id FROM lexicon.entries WHERE id=$1")
            .bind(entry)
            .fetch_one(&pool)
            .await
            .unwrap();
    *stub.revoke.lock().await = Some(actor);
    stub.replies
        .lock()
        .await
        .push_back(reply("敏感查询结果不得交付", json!([])));
    let (status, body) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/assistant"),
        &bearer,
        None,
        Some(input(entry)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(!body.to_string().contains("敏感查询结果"));
    server.abort();
}

async fn candidate_input(state: &AppState, entry: Uuid) -> Value {
    let catalog = tsz_rust::catalog::service::CatalogService::new(
        tsz_rust::catalog::repository::CatalogRepository::new(state.pool.clone()),
    )
    .catalog()
    .await
    .unwrap();
    json!({"entry_id":entry,"base_revision":1,"context_id":Uuid::now_v7(),"catalog_version":catalog.catalog_version,"headword":"bank","local_content":"未保存的填写内容"})
}
fn candidate_reply() -> Value {
    json!({"pos":[{"pos":"noun","forms":[{"form_type":"base","is_regular":true,"uk":{"spelling":"bank","dict_phonetic":"/bæŋk/"},"us":{"spelling":"bank","dict_phonetic":"/bæŋk/"}}]}],"notes":["待核查"],"model":"fixture-model","input_tokens":50,"output_tokens":100})
}
#[sqlx::test]
async fn candidate_generation_is_typed_and_does_not_save(pool: PgPool) {
    let (state, bearer, entry, stub, server) = setup(pool.clone()).await;
    let before: Value = sqlx::query_scalar(
        "SELECT to_jsonb(p) FROM lexicon.entry_editor_projection p WHERE entry_id=$1",
    )
    .bind(entry)
    .fetch_one(&pool)
    .await
    .unwrap();
    stub.replies.lock().await.push_back(candidate_reply());
    let input = candidate_input(&state, entry).await;
    let (status, body) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/assistant/form-candidates"),
        &bearer,
        None,
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["context_id"], input["context_id"]);
    assert_eq!(body["requires_human_review"], true);
    assert_eq!(body["pos"][0]["forms"][0]["uk"]["dict_phonetic"], "/bæŋk/");
    let after: Value = sqlx::query_scalar(
        "SELECT to_jsonb(p) FROM lexicon.entry_editor_projection p WHERE entry_id=$1",
    )
    .bind(entry)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    let sent = stub.inputs.lock().await;
    assert_eq!(sent[0]["headword"], "bank");
    assert!(
        sent[0]["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["pos"].as_str().is_some())
    );
    server.abort();
}
#[sqlx::test]
async fn candidates_reject_nonowner_stale_catalog_and_invalid_model_output(pool: PgPool) {
    let (state, bearer, entry, stub, server) = setup(pool.clone()).await;
    let actor = seed_admin_with_role(&pool, AdminRole::Admin).await;
    let ordinary = token(&state, actor);
    let input = candidate_input(&state, entry).await;
    assert_eq!(
        call(
            &state,
            Method::POST,
            &format!("{ROOT}/assistant/form-candidates"),
            &ordinary,
            None,
            Some(input.clone())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut stale = input.clone();
    stale["catalog_version"] = json!(-1);
    assert_eq!(
        call(
            &state,
            Method::POST,
            &format!("{ROOT}/assistant/form-candidates"),
            &bearer,
            None,
            Some(stale)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(stub.inputs.lock().await.is_empty());
    let mut invalid = candidate_reply();
    invalid["pos"][0]["forms"][0]["form_type"] = json!("execute_sql");
    stub.replies.lock().await.push_back(invalid);
    assert_eq!(
        call(
            &state,
            Method::POST,
            &format!("{ROOT}/assistant/form-candidates"),
            &bearer,
            None,
            Some(input)
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(stub.inputs.lock().await.len(), 1);
    server.abort();
}

fn user_message(body: &Value) -> Value {
    let messages = body["messages"].as_array().unwrap();
    let content = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    serde_json::from_str(content).unwrap()
}

#[sqlx::test]
async fn prepared_materials_validate_the_unsaved_draft_without_tool_rounds(pool: PgPool) {
    let (state, bearer, entry, stub, server) = setup(pool).await;
    let (status, saved) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let word = &saved["word"];
    stub.replies
        .lock()
        .await
        .push_back(reply("按校验结果先补齐词性。", json!([])));
    let mut body = input(entry);
    body["draft"] = json!({"forms": word["forms"], "meanings": word["meanings"]});
    body["extensions"] = json!(true);
    body["draft"]["meanings"]["sense_groups"] =
        json!([{ "id":Uuid::now_v7(),"name_zh":"未保存区间","name_en":"unsaved" }]);
    let (status, result) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/assistant"),
        &bearer,
        None,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["usage"]["model_calls"], 1);
    assert_eq!(result["usage"]["tool_calls"], 0);
    assert!(result["context_summary"].is_string());
    assert!(!result["field_refs"].as_array().unwrap().is_empty());
    let inputs = stub.inputs.lock().await;
    assert_eq!(
        inputs[0]["tools"],
        json!([
            "check_draft",
            "find_entries",
            "read_lexicon",
            "lookup_dictionary"
        ])
    );
    let message = user_message(&inputs[0]);
    assert_eq!(message["headword"], "bank");
    let materials = &message["server_materials"];
    assert_eq!(materials["validation"]["scope"], "当前未保存编辑");
    assert!(
        !materials["validation"]["result"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "sense_group_required")
    );
    assert!(
        message["untrusted_unsaved_content"]
            .to_string()
            .contains("未保存区间")
    );
    let persisted: Value = sqlx::query_scalar(
        "SELECT meanings FROM lexicon.entry_editor_projection WHERE entry_id=$1",
    )
    .bind(entry)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(!persisted.to_string().contains("未保存区间"));
    assert!(
        materials["validation"]["result"]["issue_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        materials["builtin_dictionary"]
            .to_string()
            .contains("financial institution")
    );
    assert_eq!(materials["catalog"]["scope"], "current_pos_details");
    assert!(
        materials["catalog"]["available_pos"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "verb")
    );
    assert!(
        !materials["catalog"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "verb")
    );
    assert!(message["untrusted_unsaved_content"]["forms"].is_object());
    assert!(
        !message["untrusted_unsaved_content"]
            .to_string()
            .contains("\"id\":")
    );
    for pos in word["forms"]["pos"].as_array().into_iter().flatten() {
        assert!(
            !message["untrusted_unsaved_content"]
                .to_string()
                .contains(pos["pos_id"].as_str().unwrap_or("absent-id"))
        );
    }
    let sources = result["sources"].as_array().unwrap();
    assert!(
        sources
            .iter()
            .any(|source| source["kind"] == "builtin_dictionary")
    );
    assert!(
        sources
            .iter()
            .any(|source| source["kind"] == "filling_rules")
    );
    server.abort();
}

#[sqlx::test]
async fn save_during_answer_delivers_with_notice_instead_of_conflict(pool: PgPool) {
    let (state, bearer, entry, stub, server) = setup(pool).await;
    *stub.bump.lock().await = Some(entry);
    stub.replies
        .lock()
        .await
        .push_back(reply("建议补充河岸义项。", json!([])));
    let (status, result) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/assistant"),
        &bearer,
        None,
        Some(input(entry)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let answer = result["answer"].as_str().unwrap();
    assert!(answer.starts_with("建议补充河岸义项。"));
    assert!(answer.contains("回答期间词条已保存过"));
    server.abort();
}

#[sqlx::test]
async fn find_entries_matches_exact_spelling_of_other_entries(pool: PgPool) {
    let (state, bearer, bank, stub, server) = setup(pool.clone()).await;
    let forms: Value = serde_json::from_str(
        &complete_v3_forms_fixture()
            .to_string()
            .replace("harbour", "colour")
            .replace("harbor", "color"),
    )
    .unwrap();
    let mut question = input(bank);
    question["draft"] = json!({"forms":forms,"meanings":{"sense_groups":[],"pos":[]}});
    stub.replies
        .lock()
        .await
        .push_back(reply("核对双拼写。", json!([])));
    let (status, result) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/assistant"),
        &bearer,
        None,
        Some(question),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let prepared = user_message(&stub.inputs.lock().await[0]);
    let terms: Vec<_> = prepared["server_materials"]["builtin_dictionary"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["spelling"].as_str().unwrap())
        .collect();
    assert_eq!(terms, vec!["colour", "color"]);
    stub.inputs.lock().await.clear();
    let river = create_legacy_v3_empty_skeleton(&state, &bearer, "river").await;
    stub.replies.lock().await.extend([
        reply(
            "",
            json!([{"id":"same","name":"find_entries","arguments":"{\"spelling\":\"bank\"}"}]),
        ),
        reply("库里已有 bank 词条。", json!([])),
    ]);
    let (status, result) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/assistant"),
        &bearer,
        None,
        Some(input(river)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let inputs = stub.inputs.lock().await;
    let tool = inputs[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(tool.contains(&bank.to_string()), "{tool}");
    let prepared = user_message(&inputs[0]);
    assert!(
        !prepared["server_materials"]["same_spelling_entries"]
            .to_string()
            .contains(&river.to_string())
    );
    server.abort();
}
