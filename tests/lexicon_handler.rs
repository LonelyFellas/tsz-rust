//! 智能词库从检测、建稿、分步保存到不可变发布的主链路契约测试。

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    lexicon::dto::SurfacePolicyNameV2,
    lexicon::normalization::{HEADWORD_NORMALIZATION_VERSION, normalize_headword},
    lexicon::surface_backfill::{
        SURFACE_WRITER_VERSION, execute_surface_cutover, run_surface_backfill,
        run_surface_cutover_preflight, run_surface_parity, surface_cutover_artifact_sha256,
    },
    platform,
    state::AppState,
};

const ROOT: &str = "/api/v1/admin/lexicon";

fn test_redis_url() -> String {
    std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_owned())
}

async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("lexicon-{}", id.simple()),
            display_name: "词库测试管理员".to_owned(),
            password_hash: "hashed-password".to_owned(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin 应成功");
    id
}

async fn seed_dictionary_word(pool: &PgPool, word: &str) {
    seed_dictionary_term(pool, word, "word", "common_unmarked").await;
}

async fn seed_dictionary_term(pool: &PgPool, term: &str, kind: &str, region_family: &str) {
    let dataset_id: i64 = if let Some(dataset_id) =
        sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status = 'active'")
            .fetch_optional(pool)
            .await
            .expect("应能查询 active dictionary dataset")
    {
        dataset_id
    } else {
        sqlx::query_scalar(
            r#"
            INSERT INTO dictionary.datasets (
                version, source_name, source_version, rules_version,
                terms_sha256, regions_sha256, status
            ) VALUES ($1, 'test', 'v1', 'v1', 'terms', 'regions', 'active')
            RETURNING id
            "#,
        )
        .bind(format!("lexicon-{term}"))
        .fetch_one(pool)
        .await
        .expect("应能插入 active dictionary dataset")
    };
    sqlx::query(
        r#"
        INSERT INTO dictionary.terms (
            dataset_id, normalized_term, term, kind, pos, status,
            sense_count, filtered_cold_sense_count, region_family
        ) VALUES ($1, $2, $2, $3, ARRAY['noun'], 'accepted', 1, 0, $4)
        "#,
    )
    .bind(dataset_id)
    .bind(term)
    .bind(kind)
    .bind(region_family)
    .execute(pool)
    .await
    .expect("应能插入 dictionary term");
}

fn token(state: &AppState, admin_id: Uuid) -> String {
    state
        .admin_token_manager
        .generate(admin_id, AdminRole::Admin.as_str())
        .expect("测试 token 应能签发")
}

async fn call(
    state: &AppState,
    method: Method,
    uri: &str,
    bearer: &str,
    idempotency_key: Option<Uuid>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    if let Some(idempotency_key) = idempotency_key {
        builder = builder.header("Idempotency-Key", idempotency_key.to_string());
    }
    let body = if let Some(body) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&body).unwrap())
    } else {
        Body::empty()
    };
    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "响应应为 JSON：{error}，body={}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, body)
}

async fn call_raw(
    state: &AppState,
    method: Method,
    uri: &str,
    bearer: &str,
    idempotency_key: Option<&str>,
    body: &[u8],
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(idempotency_key) = idempotency_key {
        builder = builder.header("Idempotency-Key", idempotency_key);
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::from(body.to_vec())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "Problem Details 响应应为 JSON：{error}，body={}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, body)
}

fn rich_text(text: &str) -> Value {
    json!({"version": 1, "text": text, "spans": [], "liaisons": []})
}

fn has_issue(body: &Value, expected_code: &str) -> bool {
    body["field_issues"]
        .as_array()
        .is_some_and(|issues| issues.iter().any(|issue| issue["code"] == expected_code))
}

async fn seed_related_search_entry(
    pool: &PgPool,
    admin_id: Uuid,
    headword: &str,
    kind: &str,
    gloss: &str,
    published: bool,
    archived: bool,
) -> (Uuid, Uuid) {
    let entry_id = Uuid::now_v7();
    let sense_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entries (
            id, language, kind, revision, headword_mode, detection_snapshot,
            created_by_admin_id, updated_by_admin_id
        ) VALUES ($1, 'en', $2, 1, 'unified', '{}', $3, $3)
        "#,
    )
    .bind(entry_id)
    .bind(kind)
    .bind(admin_id)
    .execute(pool)
    .await
    .unwrap();
    if published {
        let publication_id = Uuid::now_v7();
        let now = Utc::now();
        let snapshot = json!({
            "schema_version": 2,
            "id": entry_id,
            "language": "en",
            "kind": kind,
            "status": "published",
            "revision": 1,
            "published_revision": 1,
            "has_unpublished_changes": false,
            "headwords": {"mode": "unified", "common": headword},
            "detection_snapshot": {
                "detection_id": Uuid::now_v7(),
                "request": {"language": "en", "headword": headword},
                "normalized_headword": headword,
                "entry_kind": kind,
                "matched_dialect": "common",
                "builtin_dictionary_status": "matched",
                "smart_dictionary_status": "clear",
                "headwords": {"mode": "unified", "common": headword},
                "suggested_pos": ["noun"],
                "detected_at": now
            },
            "forms": {"pos": []},
            "meanings": {
                "sense_groups": [],
                "pos": [{
                    "pos_id": Uuid::now_v7(),
                    "grammar_structures": [],
                    "senses": [{
                        "id": sense_id,
                        "sub_pos": "",
                        "level": "A1",
                        "depends_on_context": false,
                        "definitions": [{
                            "definition_mode": "zh_definition",
                            "id": Uuid::now_v7(),
                            "content_id": Uuid::now_v7(),
                            "level": "A1",
                            "content": rich_text(gloss)
                        }],
                        "sentences": [],
                        "relations": []
                    }]
                }]
            },
            "completed_steps": ["basics", "forms", "meanings"],
            "max_reachable_step": "preview",
            "created_by": admin_id,
            "created_at": now,
            "updated_at": now,
            "published_at": now
        });
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_publications (
                id, entry_id, publication_number, source_revision, content_schema_version,
                snapshot, snapshot_hash, published_by_admin_id, published_at
            ) VALUES ($1, $2, 1, 1, 2, $3, $4, $5, $6)
            "#,
        )
        .bind(publication_id)
        .bind(entry_id)
        .bind(snapshot)
        .bind(publication_id.as_bytes().to_vec())
        .bind(admin_id)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE lexicon.entries SET current_publication_id = $2, draft_based_on_publication_id = $2 WHERE id = $1",
        )
        .bind(entry_id)
        .bind(publication_id)
        .execute(pool)
        .await
        .unwrap();
        let normalized = normalize_headword(headword).unwrap();
        for dialect_scope in ["uk", "us"] {
            sqlx::query(
                r#"
                INSERT INTO lexicon.surface_sources (
                    entry_id, source_id, source_kind, language, entry_kind,
                    dialect, dialect_scope, surface, normalized_surface,
                    normalization_version, source_revision, is_deleted,
                    content_scope, publication_id
                ) VALUES (
                    $1, $2, 'headword', 'en', $3,
                    'common', $4, $5, $6,
                    $7, 1, FALSE, 'current_publication', $8
                )
                "#,
            )
            .bind(entry_id)
            .bind(format!("headword:{entry_id}:common"))
            .bind(kind)
            .bind(dialect_scope)
            .bind(&normalized.display)
            .bind(&normalized.key)
            .bind(HEADWORD_NORMALIZATION_VERSION)
            .bind(publication_id)
            .execute(pool)
            .await
            .unwrap();
        }
    }
    if archived {
        sqlx::query("UPDATE lexicon.entries SET archived_at = now() WHERE id = $1")
            .bind(entry_id)
            .execute(pool)
            .await
            .unwrap();
    }
    (entry_id, sense_id)
}

async fn create_ready_draft(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
    headword: &str,
) -> Value {
    let dictionary_term_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM dictionary.active_terms WHERE normalized_term = $1)",
    )
    .bind(headword)
    .fetch_one(pool)
    .await
    .expect("应能检查测试词典词条");
    if !dictionary_term_exists {
        seed_dictionary_word(pool, headword).await;
    }
    let (status, detection) = call(
        state,
        Method::POST,
        &format!("{ROOT}/detections"),
        bearer,
        None,
        Some(json!({"language": "en", "headword": headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "检测失败：{detection}");
    let mut create_input = json!({
        "schema_version": 2,
        "detection_id": detection["detection_id"],
        "headwords": detection["builtin_dictionary"]["headwords"],
    });
    if let Some(surface_token) =
        detection["smart_dictionary"]["surface_match_page"]["surface_confirmation_token"].as_str()
    {
        create_input["confirmed_surface_match_token"] = json!(surface_token);
    }
    let (status, created) = call(
        state,
        Method::POST,
        &format!("{ROOT}/entries"),
        bearer,
        Some(Uuid::now_v7()),
        Some(create_input),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建失败：{created}");
    let entry_id = created["word"]["id"].as_str().unwrap();

    let mut forms = created["word"]["forms"].clone();
    forms["pos"][0]["base_form"]["variants"][0]["pronunciations"][0]["dict_phonetic"] =
        json!("/test/");
    forms["pos"][0]["base_form"]["variants"][0]["pronunciations"][0]["actual_pron"] = json!("test");
    forms["pos"][0]["form_groups"][0]["slots"] = json!([{
        "id": Uuid::now_v7(),
        "form_type": "plural",
        "variants": [{
            "id": Uuid::now_v7(),
            "dialect": "common",
            "spelling": format!("{headword}s"),
            "origin": "manual",
            "pronunciations": [{
                "id": Uuid::now_v7(),
                "dict_phonetic": "/tests/",
                "actual_pron": "tests",
                "style": "normal"
            }]
        }]
    }]);
    let forms_input = json!({
        "base_revision": 1,
        "intent": "complete",
        "content": forms,
    });
    let (mut status, mut forms_saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        bearer,
        None,
        Some(forms_input.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT
        && forms_saved["code"] == "surface_match_acknowledgement_required"
    {
        let surface_token = forms_saved["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .expect("单页 forms warning 应签发确认 token");
        let mut confirmed = forms_input;
        confirmed["confirmed_surface_match_token"] = json!(surface_token);
        (status, forms_saved) = call(
            state,
            Method::PUT,
            &format!("{ROOT}/entries/{entry_id}/steps/forms"),
            bearer,
            None,
            Some(confirmed),
        )
        .await;
    }
    assert_eq!(status, StatusCode::OK, "forms 失败：{forms_saved}");

    let mut meanings = forms_saved["word"]["meanings"].clone();
    meanings["sense_groups"][0]["name_zh"] = json!("测试含义");
    meanings["sense_groups"][0]["name_en"] = json!("Test meaning");
    meanings["pos"][0]["grammar_structures"][0]["variants"][0]["content"] =
        rich_text("used as a noun");
    meanings["pos"][0]["senses"][0]["sub_pos"] = json!("N-COUNT");
    meanings["pos"][0]["senses"][0]["frequency"] = json!("50");
    meanings["pos"][0]["senses"][0]["definitions"][0]["content"] =
        rich_text(&format!("{headword} 的释义"));
    meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["value"] =
        rich_text(&format!("A {headword} example."));
    meanings["pos"][0]["senses"][0]["sentences"][0]["zh_text"] = rich_text("测试例句。");
    let (status, meanings_saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        bearer,
        None,
        Some(json!({
            "base_revision": 2,
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "meanings 失败：{meanings_saved}");
    meanings_saved
}

async fn publish_ready(state: &AppState, bearer: &str, word: &Value) -> (StatusCode, Value) {
    call(
        state,
        Method::POST,
        &format!(
            "{ROOT}/entries/{}/publications",
            word["word"]["id"].as_str().unwrap()
        ),
        bearer,
        Some(Uuid::now_v7()),
        Some(json!({"base_revision": word["word"]["revision"]})),
    )
    .await
}

async fn publish_ready_confirming(
    state: &AppState,
    bearer: &str,
    word: &Value,
) -> (StatusCode, Value) {
    let (status, response) = publish_ready(state, bearer, word).await;
    if status != StatusCode::CONFLICT
        || response["code"] != "surface_match_acknowledgement_required"
    {
        return (status, response);
    }
    let surface_token = response["meta"]["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .expect("单页 publish warning 应签发确认 token");
    call(
        state,
        Method::POST,
        &format!(
            "{ROOT}/entries/{}/publications",
            word["word"]["id"].as_str().unwrap()
        ),
        bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": word["word"]["revision"],
            "confirmed_surface_match_token": surface_token,
        })),
    )
    .await
}

async fn prepare_duplicate_headword_test(
    pool: &PgPool,
    policies: &tsz_rust::lexicon::surface_policy::SurfacePolicyStore,
) {
    run_surface_backfill(pool).await.unwrap();
    execute_surface_cutover(
        pool,
        policies,
        SURFACE_WRITER_VERSION,
        &surface_cutover_artifact_sha256(),
    )
    .await
    .unwrap();
    policies
        .transition_exact_headword_creation(pool, true)
        .await
        .unwrap();
}

async fn restore_confirming(state: &AppState, bearer: &str, word: &Value) -> (StatusCode, Value) {
    let uri = format!("{ROOT}/entries/{}/restore", word["id"].as_str().unwrap());
    let body = json!({
        "base_revision": word["revision"],
        "base_lifecycle_revision": word["lifecycle_revision"],
    });
    let (status, response) = call(
        state,
        Method::POST,
        &uri,
        bearer,
        Some(Uuid::now_v7()),
        Some(body.clone()),
    )
    .await;
    if status != StatusCode::CONFLICT
        || response["code"] != "surface_match_acknowledgement_required"
    {
        return (status, response);
    }
    let token = response["meta"]["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .expect("单页 restore warning 应签发确认 token");
    let mut confirmed = body;
    confirmed["confirmed_surface_match_token"] = json!(token);
    call(
        state,
        Method::POST,
        &uri,
        bearer,
        Some(Uuid::now_v7()),
        Some(confirmed),
    )
    .await
}

#[sqlx::test]
async fn visibility_gate_off_allows_zero_to_one_and_same_entry_revision_but_blocks_one_to_two(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let policies = state.surface_policy_store_for_test();
    prepare_duplicate_headword_test(&pool, &policies).await;
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let first = create_ready_draft(&state, &pool, &bearer, "visibility-workspace").await;
    let (status, first_published) = publish_ready(&state, &bearer, &first).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "0→1 发布失败：{first_published}"
    );
    let second = create_ready_draft(&state, &pool, &bearer, "visibility-workspace").await;

    let first_id = first_published["word"]["id"].as_str().unwrap();
    let (status, edited) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{first_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": first_published["word"]["revision"],
            "intent": "save",
            "content": first_published["word"]["meanings"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "发布后草稿更新失败：{edited}");
    let (status, republished) = publish_ready_confirming(&state, &bearer, &edited).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "同一 active entry 新 revision 不得被 visibility gate 误拦：{republished}"
    );

    let (status, blocked) = publish_ready(&state, &bearer, &second).await;
    assert_eq!(status, StatusCode::CONFLICT, "1→2 必须被 gate-off 阻断");
    assert_eq!(
        blocked["code"],
        "multiple_active_exact_headword_publications_not_enabled"
    );
    let page = &blocked["meta"]["surface_match_page"];
    assert_eq!(page["continuation_policy"], "temporarily_disabled");
    assert!(page.get("surface_confirmation_token").is_none());
    assert!(
        page["confirmation_reasons"]
            .as_array()
            .is_some_and(|reasons| reasons
                .iter()
                .any(|reason| reason == "visibility_activation"))
    );
    let active_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.entries WHERE archived_at IS NULL AND current_publication_id IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active_count, 1, "失败发布不得留下第二条 active publication");
    assert!(
        !policies
            .multiple_active_exact_headword_publications()
            .await
            .unwrap()
            .enabled
    );
    policies
        .transition_exact_headword_creation(&pool, false)
        .await
        .unwrap();
}

#[sqlx::test]
async fn visibility_gate_off_blocks_zero_to_two_batch_atomically_and_single_restore_one_to_two(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let policies = state.surface_policy_store_for_test();
    prepare_duplicate_headword_test(&pool, &policies).await;
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let first = create_ready_draft(&state, &pool, &bearer, "restore-workspace").await;
    let (status, first_published) = publish_ready(&state, &bearer, &first).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "首条发布失败：{first_published}"
    );
    let first_id = first_published["word"]["id"].as_str().unwrap();
    let (status, first_archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{first_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": first_published["word"]["revision"],
            "base_lifecycle_revision": first_published["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "首条归档失败：{first_archived}");

    let second = create_ready_draft(&state, &pool, &bearer, "restore-workspace").await;
    let (status, second_published) = publish_ready_confirming(&state, &bearer, &second).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "active set 0→1 应成功：{second_published}"
    );
    let second_id = second_published["word"]["id"].as_str().unwrap();
    let (status, second_archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{second_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": second_published["word"]["revision"],
            "base_lifecycle_revision": second_published["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "第二条归档失败：{second_archived}");

    let batch = json!({
        "entries": [
            {
                "id": first_id,
                "base_revision": first_archived["word"]["revision"],
                "base_lifecycle_revision": first_archived["word"]["lifecycle_revision"],
            },
            {
                "id": second_id,
                "base_revision": second_archived["word"]["revision"],
                "base_lifecycle_revision": second_archived["word"]["lifecycle_revision"],
            }
        ]
    });
    let (status, blocked_batch) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/restore-batch"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(batch),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "0→2 batch 必须原子失败");
    assert_eq!(
        blocked_batch["code"],
        "multiple_active_exact_headword_publications_not_enabled"
    );
    let archived_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.entries WHERE id = ANY($1) AND archived_at IS NOT NULL",
    )
    .bind(vec![
        Uuid::parse_str(first_id).unwrap(),
        Uuid::parse_str(second_id).unwrap(),
    ])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(archived_count, 2, "batch 失败不得部分恢复");

    let (status, first_restored) =
        restore_confirming(&state, &bearer, &first_archived["word"]).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "单条 0→1 restore 应成功：{first_restored}"
    );
    let (status, blocked_second) =
        restore_confirming(&state, &bearer, &second_archived["word"]).await;
    assert_eq!(status, StatusCode::CONFLICT, "单条 1→2 restore 必须失败");
    assert_eq!(
        blocked_second["code"],
        "multiple_active_exact_headword_publications_not_enabled"
    );
    policies
        .transition_exact_headword_creation(&pool, false)
        .await
        .unwrap();
}

#[sqlx::test]
async fn gate_on_publish_uses_one_composite_token_and_reconfirms_match_and_epoch_changes(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let policies = state.surface_policy_store_for_test();
    prepare_duplicate_headword_test(&pool, &policies).await;
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let first = create_ready_draft(&state, &pool, &bearer, "composite-workspace").await;
    let (status, first_published) = publish_ready(&state, &bearer, &first).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "首条发布失败：{first_published}"
    );
    let second = create_ready_draft(&state, &pool, &bearer, "composite-workspace").await;
    let ordinary = create_ready_draft(&state, &pool, &bearer, "composite-workspaces").await;
    let first_id = first_published["word"]["id"].as_str().unwrap();
    let ordinary_id = ordinary["word"]["id"].as_str().unwrap();

    policies
        .transition_exact_headword_creation(&pool, false)
        .await
        .unwrap();
    policies
        .transition(
            &pool,
            SurfacePolicyNameV2::AllowMultipleActiveExactHeadwordPublications,
            true,
        )
        .await
        .unwrap();

    let (status, composite) = publish_ready(&state, &bearer, &second).await;
    assert_eq!(status, StatusCode::CONFLICT, "gate-on 首次发布必须确认");
    assert_eq!(composite["code"], "surface_match_acknowledgement_required");
    let page = &composite["meta"]["surface_match_page"];
    assert_eq!(
        page["policy_name"],
        "allow_multiple_active_exact_headword_publications"
    );
    assert_eq!(page["continuation_policy"], "enabled");
    let reasons = page["confirmation_reasons"].as_array().unwrap();
    assert!(
        reasons
            .iter()
            .any(|reason| reason == "visibility_activation")
    );
    assert!(
        reasons
            .iter()
            .any(|reason| reason == "unacknowledged_surface_matches")
    );
    let items = page["items"].as_array().unwrap();
    assert!(items.iter().any(|item| {
        item["existing"]["word_id"] == first_id
            && item["existing"]["source"]["content_scope"] == "current_publication"
            && item["confirmation_reasons"]
                .as_array()
                .is_some_and(|reasons| reasons.len() == 2)
    }));
    assert!(items.iter().any(|item| {
        item["existing"]["word_id"] == ordinary_id
            && item["confirmation_reasons"]
                .as_array()
                .is_some_and(|reasons| {
                    reasons.len() == 1 && reasons[0] == "unacknowledged_surface_matches"
                })
    }));
    let original_token = page["surface_confirmation_token"]
        .as_str()
        .expect("composite 终页只能签发一个 surface token")
        .to_owned();

    policies
        .transition_exact_headword_creation(&pool, true)
        .await
        .unwrap();
    let concurrent = create_ready_draft(&state, &pool, &bearer, "composite-workspace").await;
    let concurrent_id = concurrent["word"]["id"].as_str().unwrap();
    let publish_uri = format!(
        "{ROOT}/entries/{}/publications",
        second["word"]["id"].as_str().unwrap()
    );
    let (status, changed) = call(
        &state,
        Method::POST,
        &publish_uri,
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": second["word"]["revision"],
            "confirmed_surface_match_token": original_token,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "并发 match 变化必须重确认");
    assert_eq!(changed["code"], "surface_matches_changed");
    assert!(
        changed["meta"]["surface_match_page"]["items"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| { item["existing"]["word_id"] == concurrent_id }))
    );
    let changed_token = changed["meta"]["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .expect("变化后的 composite 首页应签发新 token")
        .to_owned();

    policies
        .transition(
            &pool,
            SurfacePolicyNameV2::AllowMultipleActiveExactHeadwordPublications,
            false,
        )
        .await
        .unwrap();
    let (status, disabled_policy_changed) = call(
        &state,
        Method::POST,
        &publish_uri,
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": second["word"]["revision"],
            "confirmed_surface_match_token": changed_token.clone(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        disabled_policy_changed["code"], "surface_policy_changed",
        "gate 关闭后旧 epoch token 必须先按策略变化拒绝，不能降级成 capability block"
    );
    policies
        .transition(
            &pool,
            SurfacePolicyNameV2::AllowMultipleActiveExactHeadwordPublications,
            true,
        )
        .await
        .unwrap();
    let (status, policy_changed) = call(
        &state,
        Method::POST,
        &publish_uri,
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": second["word"]["revision"],
            "confirmed_surface_match_token": changed_token,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "visibility epoch 变化必须拒绝旧 token"
    );
    assert_eq!(policy_changed["code"], "surface_policy_changed");

    let (status, refreshed) = publish_ready(&state, &bearer, &second).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(refreshed["code"], "surface_match_acknowledgement_required");
    let refreshed_token = refreshed["meta"]["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .unwrap();
    let (status, published) = call(
        &state,
        Method::POST,
        &publish_uri,
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": second["word"]["revision"],
            "confirmed_surface_match_token": refreshed_token,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "新 epoch composite token 应一次发布：{published}"
    );
    let second_id = Uuid::parse_str(second["word"]["id"].as_str().unwrap()).unwrap();
    let confirmation_actions: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT action
        FROM audit.admin_actions
        WHERE resource_id = $1
          AND action IN (
              'lexicon.surface_warning.acknowledge_command',
              'lexicon.visibility_activation.acknowledge'
          )
        ORDER BY action
        "#,
    )
    .bind(second_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        confirmation_actions,
        [
            "lexicon.surface_warning.acknowledge_command",
            "lexicon.visibility_activation.acknowledge",
        ],
        "composite 命令应在同一事务内分别留下两个 reason 的审计证据"
    );

    policies
        .transition(
            &pool,
            SurfacePolicyNameV2::AllowMultipleActiveExactHeadwordPublications,
            false,
        )
        .await
        .unwrap();
    policies
        .transition_exact_headword_creation(&pool, false)
        .await
        .unwrap();
}

#[sqlx::test]
async fn historical_publication_activation_obeys_visibility_gate_and_command_confirmation(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let policies = state.surface_policy_store_for_test();
    prepare_duplicate_headword_test(&pool, &policies).await;
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let first = create_ready_draft(&state, &pool, &bearer, "activation-workspace").await;
    let (status, first_published) = publish_ready(&state, &bearer, &first).await;
    assert_eq!(status, StatusCode::CREATED);
    let first_id = first_published["word"]["id"].as_str().unwrap();
    let (status, first_archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{first_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": first_published["word"]["revision"],
            "base_lifecycle_revision": first_published["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "首条归档失败：{first_archived}");

    let second = create_ready_draft(&state, &pool, &bearer, "activation-workspace").await;
    let (status, second_published) = publish_ready_confirming(&state, &bearer, &second).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "第二条 0→1 发布失败：{second_published}"
    );
    let second_id = Uuid::parse_str(second_published["word"]["id"].as_str().unwrap()).unwrap();
    let second_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(second_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id = NULL WHERE id = $1")
        .bind(second_id)
        .execute(&pool)
        .await
        .unwrap();

    let (status, first_restored) =
        restore_confirming(&state, &bearer, &first_archived["word"]).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "首条 0→1 restore 失败：{first_restored}"
    );
    let activation_uri =
        format!("{ROOT}/entries/{second_id}/publications/{second_publication_id}/activate");
    let activation_body = json!({
        "base_revision": second_published["word"]["revision"],
        "base_lifecycle_revision": second_published["word"]["lifecycle_revision"],
    });
    let (status, disabled) = call(
        &state,
        Method::POST,
        &activation_uri,
        &bearer,
        Some(Uuid::now_v7()),
        Some(activation_body.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "activation 1→2 必须被 gate-off 阻断"
    );
    assert_eq!(
        disabled["code"],
        "multiple_active_exact_headword_publications_not_enabled"
    );
    let still_inactive: Option<Uuid> =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(second_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        still_inactive.is_none(),
        "失败 activation 不得切换 current publication"
    );

    policies
        .transition(
            &pool,
            SurfacePolicyNameV2::AllowMultipleActiveExactHeadwordPublications,
            true,
        )
        .await
        .unwrap();
    let (status, required) = call(
        &state,
        Method::POST,
        &activation_uri,
        &bearer,
        Some(Uuid::now_v7()),
        Some(activation_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(required["code"], "surface_match_acknowledgement_required");
    let activation_token = required["meta"]["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .expect("activation visibility snapshot 应签发命令 token");
    let mut confirmed = activation_body;
    confirmed["confirmed_surface_match_token"] = json!(activation_token);
    let success_key = Uuid::now_v7();
    let (status, activated) = call(
        &state,
        Method::POST,
        &activation_uri,
        &bearer,
        Some(success_key),
        Some(confirmed.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "activation 确认后失败：{activated}");
    assert_eq!(
        activated["word"]["published_revision"],
        second_published["word"]["revision"]
    );
    let (status, replayed) = call(
        &state,
        Method::POST,
        &activation_uri,
        &bearer,
        Some(success_key),
        Some(confirmed),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed, activated, "activation 成功响应必须幂等重放");
    let visibility_audit_count: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM audit.admin_actions
        WHERE resource_id = $1
          AND action = 'lexicon.visibility_activation.acknowledge'
        "#,
    )
    .bind(second_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        visibility_audit_count, 1,
        "activation 幂等重放不得重复写 visibility 确认审计"
    );

    policies
        .transition(
            &pool,
            SurfacePolicyNameV2::AllowMultipleActiveExactHeadwordPublications,
            false,
        )
        .await
        .unwrap();
    policies
        .transition_exact_headword_creation(&pool, false)
        .await
        .unwrap();
}

#[sqlx::test]
async fn historical_publication_activation_increments_lifecycle_revision_across_a_b_a(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let draft = create_ready_draft(&state, &pool, &bearer, "activation-cycle").await;
    let (status, first_published) = publish_ready(&state, &bearer, &draft).await;
    assert_eq!(status, StatusCode::CREATED);
    let entry_id = Uuid::parse_str(first_published["word"]["id"].as_str().unwrap()).unwrap();

    let (status, edited) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": first_published["word"]["revision"],
            "intent": "complete",
            "content": first_published["word"]["meanings"],
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "创建第二个 publication 草稿失败：{edited}"
    );
    let (status, second_published) = publish_ready(&state, &bearer, &edited).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "第二个 publication 发布失败：{second_published}"
    );

    let publications: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM lexicon.entry_publications WHERE entry_id = $1 ORDER BY publication_number",
    )
    .bind(entry_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(publications.len(), 2);
    let mut lifecycle_revision = second_published["word"]["lifecycle_revision"]
        .as_i64()
        .unwrap();
    for publication_id in [publications[0], publications[1], publications[0]] {
        let (status, activated) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/entries/{entry_id}/publications/{publication_id}/activate"),
            &bearer,
            Some(Uuid::now_v7()),
            Some(json!({
                "base_revision": second_published["word"]["revision"],
                "base_lifecycle_revision": lifecycle_revision,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "A→B→A activation 失败：{activated}");
        lifecycle_revision += 1;
        assert_eq!(
            activated["word"]["lifecycle_revision"], lifecycle_revision,
            "每次切换 current publication 都必须推进 lifecycle revision"
        );
    }
    let activation_events: Vec<i64> = sqlx::query_scalar(
        r#"
        SELECT aggregate_revision
        FROM platform.outbox_events
        WHERE aggregate_id = $1 AND event_type = 'lexicon.publication_activated'
        ORDER BY aggregate_revision
        "#,
    )
    .bind(entry_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(activation_events, vec![2, 3, 4]);
}

#[sqlx::test]
async fn historical_publication_activation_rejects_missing_current_inbound_sense(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_ready = create_ready_draft(&state, &pool, &bearer, "activation-target").await;
    let (status, first_published) = publish_ready(&state, &bearer, &target_ready).await;
    assert_eq!(status, StatusCode::CREATED);
    let target_id = Uuid::parse_str(first_published["word"]["id"].as_str().unwrap()).unwrap();
    let first_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(target_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    let mut meanings = first_published["word"]["meanings"].clone();
    let mut added_sense = meanings["pos"][0]["senses"][0].clone();
    let added_sense_id = Uuid::now_v7();
    added_sense["id"] = json!(added_sense_id);
    added_sense["definitions"][0]["id"] = json!(Uuid::now_v7());
    added_sense["definitions"][0]["content_id"] = json!(Uuid::now_v7());
    added_sense["definitions"][0]["content"] = rich_text("仅第二版存在的词义");
    added_sense["sentences"][0]["id"] = json!(Uuid::now_v7());
    added_sense["sentences"][0]["en_text"]["common"]["id"] = json!(Uuid::now_v7());
    added_sense["sentences"][0]["zh_text_id"] = json!(Uuid::now_v7());
    added_sense["sentences"][0]["links"][0]["sense_id"] = json!(added_sense_id);
    meanings["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .push(added_sense);
    let (status, target_edited) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": first_published["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "添加第二版词义失败：{target_edited}"
    );
    let (status, second_published) = publish_ready(&state, &bearer, &target_edited).await;
    assert_eq!(status, StatusCode::CREATED);

    let source_ready = create_ready_draft(&state, &pool, &bearer, "activation-source").await;
    let source_id = source_ready["word"]["id"].as_str().unwrap();
    let mut source_meanings = source_ready["word"]["meanings"].clone();
    source_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": target_id,
        "target_sense_id": added_sense_id,
        "score": "80.00"
    }]);
    let (status, source_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source_ready["word"]["revision"],
            "intent": "complete",
            "content": source_meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "保存当前入站引用失败：{source_saved}"
    );
    let (status, source_published) = publish_ready(&state, &bearer, &source_saved).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "发布当前入站引用失败：{source_published}"
    );

    let (status, blocked) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/publications/{first_publication_id}/activate"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": second_published["word"]["revision"],
            "base_lifecycle_revision": second_published["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(blocked["field_issues"].as_array().is_some_and(|issues| {
        issues.iter().any(|issue| {
            issue["code"] == "sense_has_inbound_publication_refs"
                && issue["node_id"] == added_sense_id.to_string()
        })
    }));
}

#[sqlx::test]
async fn b4_backfill_is_repeatable_and_cutover_requires_clean_parity(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let policies = state.surface_policy_store_for_test();
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let draft = create_ready_draft(&state, &pool, &bearer, "workspace").await;
    let entry_id = Uuid::parse_str(draft["word"]["id"].as_str().unwrap()).unwrap();

    sqlx::query(
        "DELETE FROM platform.outbox_events WHERE aggregate_id = $1 AND aggregate_type = 'lexicon.surface_projection'",
    )
    .bind(entry_id)
    .execute(&pool)
    .await
    .unwrap();
    let missing_outbox = run_surface_parity(&pool).await.unwrap();
    assert_eq!(missing_outbox.outbox_lag, 1);
    assert!(
        !missing_outbox.ready,
        "投影存在但 outbox 缺失时必须 fail closed"
    );

    sqlx::query("DELETE FROM lexicon.surface_sources WHERE entry_id = $1")
        .bind(entry_id)
        .execute(&pool)
        .await
        .unwrap();
    let before = run_surface_parity(&pool).await.unwrap();
    assert!(!before.ready);
    assert!(!before.missing_rows.is_empty());
    let blocked = run_surface_cutover_preflight(&pool, &policies, SURFACE_WRITER_VERSION)
        .await
        .unwrap();
    assert!(!blocked.ready);

    let first = run_surface_backfill(&pool).await.unwrap();
    assert_eq!(first.scanned_entries, 1);
    assert_eq!(first.changed_entries, 1);
    assert!(first.parity.ready, "backfill 后必须零差异: {first:?}");
    assert!(first.parity.counts.iter().any(|count| {
        count.lifecycle == "draft"
            && count.content_scope == "draft"
            && count.expected == count.actual
            && count.actual >= 4
    }));

    let plural_scopes: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT dialect_scope
        FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_scope = 'draft'
          AND source_kind = 'form' AND form_type = 'plural'
          AND normalized_surface = 'workspaces' AND is_deleted = FALSE
        ORDER BY dialect_scope
        "#,
    )
    .bind(entry_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(plural_scopes, ["uk", "us"]);

    let second = run_surface_backfill(&pool).await.unwrap();
    assert_eq!(second.changed_entries, 0, "重复 backfill 必须是 no-op");
    assert_eq!(
        second.parity.expected_checksum,
        first.parity.expected_checksum
    );
    assert_eq!(second.parity.actual_checksum, first.parity.actual_checksum);

    let preflight = run_surface_cutover_preflight(&pool, &policies, SURFACE_WRITER_VERSION)
        .await
        .unwrap();
    assert!(
        preflight.ready,
        "clean parity 应允许 cutover: {preflight:?}"
    );
    assert!(preflight.legacy_unique_present_before);
    assert!(preflight.non_unique_lookup_present);

    policies
        .transition_exact_headword_creation(&pool, true)
        .await
        .unwrap();
    let (status, warning) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "workspace"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "同名检测失败：{warning}");
    assert_eq!(warning["smart_dictionary"]["status"], "warning");
    let confirmation_token =
        warning["smart_dictionary"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .expect("终页 warning 必须包含确认 token");
    let duplicate_key = Uuid::now_v7();
    let duplicate_body = json!({
        "schema_version": 2,
        "detection_id": warning["detection_id"],
        "headwords": warning["builtin_dictionary"]["headwords"],
        "confirmed_surface_match_token": confirmation_token,
    });
    let (status, unique_conflict) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(duplicate_key),
        Some(duplicate_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(unique_conflict["code"], "duplicate_word");
    let (status, unique_conflict_replay) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(duplicate_key),
        Some(duplicate_body),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        unique_conflict_replay, unique_conflict,
        "UNIQUE 冲突回滚后的业务 409 必须按同一 Idempotency-Key 原样重放"
    );
    let workspace_entries: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT entry_id) FROM lexicon.entry_headword_keys WHERE normalized_headword = 'workspace'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(workspace_entries, 1, "失败事务不得留下第二个词条");

    let enabled_preflight = run_surface_cutover_preflight(&pool, &policies, SURFACE_WRITER_VERSION)
        .await
        .unwrap();
    assert!(!enabled_preflight.ready);
    assert!(enabled_preflight.creation_policy.enabled);
    assert!(!enabled_preflight.publication_policy.enabled);
    assert!(
        enabled_preflight
            .blocking_reasons
            .contains(&"exact_headword_creation_policy_enabled".to_owned())
    );
    let enabled_cutover = execute_surface_cutover(
        &pool,
        &policies,
        SURFACE_WRITER_VERSION,
        &surface_cutover_artifact_sha256(),
    )
    .await
    .unwrap_err();
    assert!(enabled_cutover.to_string().contains("policy_enabled"));
    policies
        .transition_exact_headword_creation(&pool, false)
        .await
        .unwrap();

    let wrong_writer = run_surface_cutover_preflight(&pool, &policies, "surface-writer-v0")
        .await
        .unwrap();
    assert!(!wrong_writer.ready);
    assert!(
        wrong_writer
            .blocking_reasons
            .contains(&"writer_version_mismatch".to_owned())
    );
    let wrong_hash =
        execute_surface_cutover(&pool, &policies, SURFACE_WRITER_VERSION, "not-reviewed")
            .await
            .unwrap_err();
    assert!(wrong_hash.to_string().contains("artifact hash mismatch"));

    let cutover = execute_surface_cutover(
        &pool,
        &policies,
        SURFACE_WRITER_VERSION,
        &surface_cutover_artifact_sha256(),
    )
    .await
    .unwrap();
    assert!(cutover.executed);
    assert!(!cutover.legacy_unique_present_after);
    let entry_local_unique: bool = sqlx::query_scalar(
        "SELECT to_regclass('lexicon.lexicon_entry_headwords_entry_dialect_key') IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(entry_local_unique, "entry 内 headword/dialect 约束必须保留");
}

#[sqlx::test]
async fn b4_backfill_keeps_archived_current_publication_and_never_revives_newer_tombstone(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let ready = create_ready_draft(&state, &pool, &bearer, "archivework").await;
    let (status, published) = publish_ready(&state, &bearer, &ready).await;
    assert_eq!(status, StatusCode::CREATED, "发布失败：{published}");
    let entry_id = Uuid::parse_str(published["word"]["id"].as_str().unwrap()).unwrap();
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": published["word"]["revision"],
            "base_lifecycle_revision": published["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "归档失败：{archived}");

    sqlx::query("DELETE FROM lexicon.surface_sources WHERE entry_id = $1")
        .bind(entry_id)
        .execute(&pool)
        .await
        .unwrap();
    let backfill = run_surface_backfill(&pool).await.unwrap();
    assert!(
        backfill.parity.ready,
        "archived backfill 应零差异: {backfill:?}"
    );
    assert!(backfill.parity.counts.iter().any(|count| {
        count.lifecycle == "archived"
            && count.content_scope == "draft"
            && count.expected == count.actual
            && count.actual > 0
    }));
    assert!(backfill.parity.counts.iter().any(|count| {
        count.lifecycle == "archived"
            && count.content_scope == "current_publication"
            && count.expected == count.actual
            && count.actual > 0
    }));

    let tombstoned_source: String = sqlx::query_scalar(
        r#"
        UPDATE lexicon.surface_sources
        SET is_deleted = TRUE, source_revision = 999999,
            event_offset = nextval('lexicon.surface_projection_event_offset_seq')
        WHERE entry_id = $1 AND content_scope = 'draft'
          AND source_kind = 'form' AND form_type = 'plural'
        RETURNING source_id
        "#,
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let stale = run_surface_backfill(&pool).await.unwrap();
    assert!(
        !stale.parity.ready,
        "较新 tombstone 必须阻止旧 backfill 宣称 ready"
    );
    assert!(
        stale
            .parity
            .missing_rows
            .iter()
            .any(|row| row.source_id == tombstoned_source)
    );
    let still_deleted: bool = sqlx::query_scalar(
        "SELECT bool_and(is_deleted) FROM lexicon.surface_sources WHERE source_id = $1 AND content_scope = 'draft'",
    )
    .bind(tombstoned_source)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(still_deleted, "迟到 backfill 不得复活较新 tombstone");
}

#[sqlx::test]
async fn lexicon_editor_flow_is_revision_safe_idempotent_and_publishable(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let headword = format!("codex{}", admin_id.simple());
    seed_dictionary_word(&pool, &headword).await;

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "检测失败：{detection}");
    assert_eq!(detection["builtin_dictionary"]["status"], "matched");
    assert_eq!(
        detection["builtin_dictionary"]["provider"],
        json!({"name": "test", "version": "v1"})
    );
    assert_eq!(
        detection["builtin_dictionary"]["coverage"],
        json!({
            "forms": "partial",
            "pronunciations": "missing",
            "meanings": "missing",
            "examples": "missing",
            "frequency": "missing"
        })
    );
    assert_eq!(
        detection["builtin_dictionary"]["provenance"]["forms"],
        json!({"name": "test", "version": "v1"})
    );
    assert!(detection["builtin_dictionary"]["provenance"]["pronunciations"].is_null());
    assert!(
        detection["builtin_dictionary"]["suggested_meanings"]["pos"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(detection["builtin_dictionary"]["suggested_frequency"].is_null());
    assert_eq!(detection["smart_dictionary"]["status"], "clear");

    let create_body = json!({
        "schema_version": 2,
        "detection_id": detection["detection_id"],
        "headwords": detection["builtin_dictionary"]["headwords"],
    });
    let (status, missing_key) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        None,
        Some(create_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(missing_key["field"], "idempotency_key");

    let create_key = Uuid::now_v7();
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(create_key),
        Some(create_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建失败：{created}");
    assert_eq!(created["word"]["revision"], 1);
    let entry_id = Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap();

    let created_headword_surfaces: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*) FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_scope = 'draft'
          AND source_kind = 'headword' AND is_deleted = FALSE
        "#,
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        created_headword_surfaces, 2,
        "common headword 必须原子展开为 UK/US 两个 scope"
    );

    let (status, replayed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(create_key),
        Some(create_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replayed, created, "创建响应丢失后应可原样重放");

    let consumed_key = Uuid::now_v7();
    let consumed_body = create_body;
    let (status, consumed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(consumed_key),
        Some(consumed_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(consumed["code"], "detection_expired");
    let (status, consumed_replay) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(consumed_key),
        Some(consumed_body),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(
        consumed_replay, consumed,
        "丢失的 consumed 410 必须原样重放"
    );

    let (status, draft_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": headword})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "草稿词条检测失败：{draft_detection}"
    );
    assert_eq!(draft_detection["smart_dictionary"]["status"], "warning");
    assert!(
        draft_detection["smart_dictionary"]["duplicates"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert!(
        draft_detection["smart_dictionary"]["surface_match_page"]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| {
                item["match_category"] == "exact_headword" && item["existing"]["status"] == "draft"
            }))
    );
    let exact_gate_key = Uuid::now_v7();
    let exact_gate_body = json!({
        "schema_version": 2,
        "detection_id": draft_detection["detection_id"],
        "headwords": draft_detection["builtin_dictionary"]["headwords"],
    });
    let (status, exact_gate_off) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(exact_gate_key),
        Some(exact_gate_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        exact_gate_off["code"],
        "exact_headword_creation_temporarily_disabled"
    );
    let (status, exact_gate_replay) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(exact_gate_key),
        Some(exact_gate_body),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(exact_gate_replay, exact_gate_off, "丢失的 409 必须原样重放");

    let mut forms = created["word"]["forms"].clone();
    forms["pos"][0]["base_form"]["variants"][0]["pronunciations"][0]["dict_phonetic"] =
        json!("/kəʊdɛks/");
    forms["pos"][0]["base_form"]["variants"][0]["pronunciations"][0]["actual_pron"] =
        json!("kəʊdɛks");
    forms["pos"][0]["form_groups"][0]["slots"] = json!([{
        "id": Uuid::now_v7(),
        "form_type": "plural",
        "variants": [{
            "id": Uuid::now_v7(),
            "dialect": "common",
            "spelling": format!("{headword}s"),
            "origin": "manual",
            "pronunciations": [{
                "id": Uuid::now_v7(),
                "dict_phonetic": "/kəʊdɛksɪz/",
                "actual_pron": "kəʊdɛksɪz",
                "style": "normal"
            }]
        }]
    }]);
    let (status, forms_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": 1,
            "intent": "complete",
            "content": forms,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "完成 forms 失败：{forms_saved}");
    assert_eq!(forms_saved["word"]["revision"], 2);
    assert_eq!(forms_saved["word"]["max_reachable_step"], "meanings");

    let plural_surfaces: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT dialect_scope FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_scope = 'draft'
          AND source_kind = 'form' AND form_type = 'plural'
          AND normalized_surface = $2 AND is_deleted = FALSE
        ORDER BY dialect_scope
        "#,
    )
    .bind(entry_id)
    .bind(format!("{headword}s"))
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(plural_surfaces, ["uk", "us"]);

    let plural_headword = format!("{headword}s");
    seed_dictionary_word(&pool, &plural_headword).await;
    let (status, plural_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": plural_headword})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "复数词形检测失败：{plural_detection}"
    );
    assert_eq!(plural_detection["smart_dictionary"]["status"], "warning");
    assert!(
        plural_detection["smart_dictionary"]["surface_match_page"]["items"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| item["match_category"] == "headword_form"))
    );
    assert!(
        plural_detection["smart_dictionary"]["surface_match_page"]["surface_confirmation_token"]
            .is_string()
    );
    let plural_create = json!({
        "schema_version": 2,
        "detection_id": plural_detection["detection_id"],
        "headwords": plural_detection["builtin_dictionary"]["headwords"],
    });
    let acknowledgement_key = Uuid::now_v7();
    let (status, acknowledgement_required) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(acknowledgement_key),
        Some(plural_create.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        acknowledgement_required["code"],
        "surface_match_acknowledgement_required"
    );
    let (status, acknowledgement_replay) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(acknowledgement_key),
        Some(plural_create.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        acknowledgement_replay, acknowledgement_required,
        "丢失的 acknowledgement 409 必须保留同一 snapshot/token 原样重放"
    );
    let surface_token =
        acknowledgement_required["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .expect("409 warning 的终页必须签发新 token")
            .to_owned();

    let mut confirmed_plural_create = plural_create;
    confirmed_plural_create["confirmed_surface_match_token"] = json!(surface_token);
    let (status, plural_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(confirmed_plural_create),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "确认复数 warning 后应允许创建：{plural_created}"
    );
    assert_ne!(plural_created["word"]["id"], entry_id.to_string());
    assert_eq!(
        plural_created["word"]["detection_snapshot"]["smart_dictionary_status"],
        "warning"
    );
    assert_eq!(
        plural_created["word"]["detection_snapshot"]["surface_warning"]["acknowledged"],
        true
    );
    let acknowledgement_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.entry_surface_acknowledgements WHERE entry_id = $1",
    )
    .bind(Uuid::parse_str(plural_created["word"]["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(acknowledgement_count, 1);

    let (status, conflict) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": 1,
            "intent": "save",
            "content": forms_saved["word"]["forms"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "revision_conflict");
    assert_eq!(conflict["meta"]["current_revision"], 2);

    let mut meanings = forms_saved["word"]["meanings"].clone();
    let definition_content_id =
        meanings["pos"][0]["senses"][0]["definitions"][0]["content_id"].clone();
    let sentence_en_text_id =
        meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["id"].clone();
    let sentence_zh_text_id = meanings["pos"][0]["senses"][0]["sentences"][0]["zh_text_id"].clone();
    meanings["sense_groups"][0]["name_zh"] = json!("核心含义");
    meanings["sense_groups"][0]["name_en"] = json!("Core meaning");
    meanings["pos"][0]["grammar_structures"][0]["variants"][0]["content"] = json!({
        "version": 2,
        "text": "used as a test noun",
        "annotations": [
            {"type": "emphasis", "start": 2, "end": 4, "level": "strong"},
            {"type": "emphasis", "start": 0, "end": 2, "level": "strong"}
        ]
    });
    meanings["pos"][0]["senses"][0]["sub_pos"] = json!("N-COUNT");
    meanings["pos"][0]["senses"][0]["frequency"] = json!("50.00");
    meanings["pos"][0]["senses"][0]["definitions"][0]["content"] = rich_text("测试词");
    meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["value"] =
        rich_text(&format!("This is a {headword}."));
    meanings["pos"][0]["senses"][0]["sentences"][0]["zh_text"] = rich_text("这是一个测试词。");

    let mut invalid_rich_text_meanings = meanings.clone();
    invalid_rich_text_meanings["pos"][0]["grammar_structures"][0]["variants"][0]["content"] = json!({
        "version": 2,
        "text": "short",
        "annotations": [
            {"type": "liaison", "start": 0, "end": 99}
        ]
    });
    let (status, invalid_rich_text) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": 2,
            "intent": "save",
            "content": invalid_rich_text_meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "非法 V2 富文本在保存草稿时也必须返回 422：{invalid_rich_text}"
    );

    let mut invalid_focus_meanings = meanings.clone();
    invalid_focus_meanings["pos"][0]["senses"][0]["sentences"][0]["links"][0]["sense_id"] =
        json!(Uuid::now_v7());
    let (status, invalid_focus) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": 2,
            "intent": "save",
            "content": invalid_focus_meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "草稿保存也必须在写 FK 前拒绝伪造 focus：{invalid_focus}"
    );
    assert!(
        invalid_focus["field_issues"]
            .as_array()
            .is_some_and(|issues| issues
                .iter()
                .any(|issue| issue["code"] == "sentence_incomplete"))
    );

    let (status, meanings_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": 2,
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "完成 meanings 失败：{meanings_saved}"
    );
    assert_eq!(meanings_saved["word"]["revision"], 3);
    assert_eq!(meanings_saved["word"]["max_reachable_step"], "preview");
    assert_eq!(
        meanings_saved["word"]["meanings"]["pos"][0]["grammar_structures"][0]["variants"][0]["content"]
            ["annotations"],
        json!([{"type": "emphasis", "start": 0, "end": 4, "level": "strong"}]),
        "服务端应持久化 canonical V2 富文本"
    );
    assert_eq!(
        meanings_saved["word"]["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content_id"],
        definition_content_id
    );
    assert_eq!(
        meanings_saved["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]
            ["id"],
        sentence_en_text_id
    );
    assert_eq!(
        meanings_saved["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["zh_text_id"],
        sentence_zh_text_id
    );

    let (status, validation) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/validate"),
        &bearer,
        None,
        Some(json!({"base_revision": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "校验失败：{validation}");
    assert_eq!(validation["valid"], true);

    let (status, publish_warning) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({"base_revision": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        publish_warning["code"],
        "surface_match_acknowledgement_required"
    );
    let publish_surface_token =
        publish_warning["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .expect("发布普通词形 warning 应签发 token")
            .to_owned();
    let publish_key = Uuid::now_v7();
    let publish_input = json!({
        "base_revision": 3,
        "confirmed_surface_match_token": publish_surface_token,
    });
    let (status, published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(publish_key),
        Some(publish_input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "发布失败：{published}");
    assert_eq!(published["word"]["status"], "published");
    assert!(published["word"]["published_at"].is_string());

    let current_publication_surfaces: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM lexicon.surface_sources source
        JOIN lexicon.entries entry ON entry.id = source.entry_id
        WHERE source.entry_id = $1
          AND source.content_scope = 'current_publication'
          AND source.publication_id = entry.current_publication_id
          AND source.is_deleted = FALSE
        "#,
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        current_publication_surfaces >= 4,
        "发布必须在同一事务写 current publication surface"
    );

    let (status, published_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": headword})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "已发布词条检测失败：{published_detection}"
    );
    assert_eq!(published_detection["smart_dictionary"]["status"], "warning");
    assert!(
        published_detection["smart_dictionary"]["surface_match_page"]["items"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| item["existing"]["status"] == "published"))
    );

    let (status, publish_replay) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(publish_key),
        Some(publish_input),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(publish_replay, published, "发布命令必须可幂等重放");

    let publication_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publications WHERE entry_id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(publication_count, 1);
    let outbox_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox_events WHERE aggregate_id = $1 AND event_type = 'lexicon.entry_published'",
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(outbox_count, 1);
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit.admin_actions WHERE resource_id = $1 ORDER BY occurred_at, id",
    )
    .bind(entry_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        actions,
        [
            "lexicon.entry.create",
            "lexicon.entry.save",
            "lexicon.entry.save",
            "lexicon.entry.publish",
            "lexicon.surface_warning.acknowledge_command",
        ]
    );
    let missing_pronunciation_hashes: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM lexicon.entry_publication_nodes
        WHERE entry_id = $1 AND node_type = 'pronunciation' AND content_hash IS NULL
        "#,
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(missing_pronunciation_hashes, 0);
}

#[sqlx::test]
async fn lexicon_expiry_pagination_and_form_storage_boundaries_are_safe(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis.clone());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let first_headword = format!("expiry{}", admin_id.simple());
    seed_dictionary_word(&pool, &first_headword).await;

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": first_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "检测失败：{detection}");
    let detection_id = Uuid::parse_str(detection["detection_id"].as_str().unwrap()).unwrap();
    let detection_key = format!("lexicon:detection:{admin_id}:{detection_id}");
    let mut redis_connection = redis.get().await.expect("应能获取 Redis 连接");
    let redis_ttl: i64 = deadpool_redis::redis::cmd("TTL")
        .arg(&detection_key)
        .query_async(&mut redis_connection)
        .await
        .expect("应能读取 detection TTL");
    assert!(
        redis_ttl > 5 * 60,
        "Redis 应在五分钟逻辑过期后继续保留 detection，实际 TTL={redis_ttl}"
    );

    let expired_detection_id = Uuid::now_v7();
    let mut expired_detection = detection.clone();
    expired_detection["detection_id"] = json!(expired_detection_id);
    expired_detection["expires_at"] = json!("2000-01-01T00:00:00Z");
    deadpool_redis::redis::cmd("SET")
        .arg(format!(
            "lexicon:detection:{admin_id}:{expired_detection_id}"
        ))
        .arg(serde_json::to_string(&expired_detection).unwrap())
        .arg("EX")
        .arg(60 * 60)
        .query_async::<()>(&mut redis_connection)
        .await
        .expect("应能写入逻辑已过期但仍保留的 detection");
    let (status, expired) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": expired_detection_id,
            "headwords": detection["builtin_dictionary"]["headwords"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "逻辑过期应返回 410：{expired}");
    assert_eq!(expired["code"], "detection_expired");

    let create_key = Uuid::now_v7();
    let first_create_body = json!({
        "schema_version": 2,
        "detection_id": detection_id,
        "headwords": detection["builtin_dictionary"]["headwords"],
    });
    let (status, first_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(create_key),
        Some(first_create_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "首次创建失败：{first_created}");
    let (status, replayed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(create_key),
        Some(first_create_body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replayed, first_created);

    deadpool_redis::redis::cmd("SET")
        .arg(&detection_key)
        .arg(serde_json::to_string(&detection).unwrap())
        .arg("EX")
        .arg(60 * 60)
        .query_async::<()>(&mut redis_connection)
        .await
        .expect("应能模拟创建提交后 Redis 删除失败");
    let (status, reused_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection_id,
            "headwords": detection["builtin_dictionary"]["headwords"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(reused_detection["code"], "detection_expired");
    let duplicate_entries: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.entries WHERE detection_snapshot->>'detection_id' = $1",
    )
    .bind(detection_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(duplicate_entries, 1, "同一 detection 只能创建一个词条");

    sqlx::query(
        r#"
        UPDATE platform.idempotency_records
        SET created_at = now() - interval '2 days',
            expires_at = now() - interval '1 day'
        WHERE scope = 'lexicon.entry.create'
          AND actor_id = $1
          AND idempotency_key = $2
        "#,
    )
    .bind(admin_id)
    .bind(create_key)
    .execute(&pool)
    .await
    .expect("应能把幂等记录推进到过期状态");

    let second_headword = format!("reuse{}", admin_id.simple());
    seed_dictionary_word(&pool, &second_headword).await;
    let (status, second_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": second_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "第二次检测失败：{second_detection}");
    let (status, second_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(create_key),
        Some(json!({
            "schema_version": 2,
            "detection_id": second_detection["detection_id"],
            "headwords": second_detection["builtin_dictionary"]["headwords"],
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "过期幂等键应可原子清理后复用：{second_created}"
    );
    assert_ne!(second_created["word"]["id"], first_created["word"]["id"]);

    let (status, empty_page) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?page=4294967295&page_size=100"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "超大页码不应溢出：{empty_page}");
    assert_eq!(empty_page["words"], json!([]));
    assert_eq!(empty_page["page"]["total"], 2);

    let entry_id = Uuid::parse_str(second_created["word"]["id"].as_str().unwrap()).unwrap();
    let mut invalid_forms = second_created["word"]["forms"].clone();
    invalid_forms["pos"][0]["base_form"]["variants"][0]["spelling"] =
        json!(format!(" {} ", "x".repeat(201)));
    invalid_forms["pos"][0]["base_form"]["variants"][0]["pronunciations"][0]["dict_phonetic"] =
        json!("d".repeat(201));
    invalid_forms["pos"][0]["base_form"]["variants"][0]["pronunciations"][0]["actual_pron"] =
        json!("a".repeat(201));
    let (status, invalid) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": 1,
            "intent": "save",
            "content": invalid_forms,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "违反存储约束应返回 422 而非数据库 500：{invalid}"
    );
    let issue_codes = invalid["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|issue| issue["code"].as_str())
        .collect::<Vec<_>>();
    for expected in [
        "spelling_not_trimmed",
        "spelling_too_long",
        "dict_phonetic_too_long",
        "actual_pron_too_long",
    ] {
        assert!(
            issue_codes.contains(&expected),
            "缺少字段级问题 {expected}：{invalid}"
        );
    }

    let mut cross_entry_forms = second_created["word"]["forms"].clone();
    cross_entry_forms["pos"][0]["base_form"]["id"] =
        first_created["word"]["forms"]["pos"][0]["base_form"]["id"].clone();
    let (status, collision) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": 1,
            "intent": "save",
            "content": cross_entry_forms,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "复用其他词条节点 ID 应返回 422：{collision}"
    );
    assert!(
        collision["field_issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["code"] == "node_id_reused")
    );

    let mut cross_step_forms = second_created["word"]["forms"].clone();
    cross_step_forms["pos"][0]["base_form"]["id"] =
        second_created["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();
    let (status, collision) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": 1,
            "intent": "save",
            "content": cross_step_forms,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "跨步骤复用节点 ID 应返回 422：{collision}"
    );
    assert!(
        collision["field_issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["code"] == "node_id_reused")
    );
}

#[sqlx::test]
async fn published_sense_references_are_resolved_snapshotted_and_protected(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_headword = format!("target{}", admin_id.simple());
    let mut target_ready = create_ready_draft(&state, &pool, &bearer, &target_headword).await;
    let target_entry_id = Uuid::parse_str(target_ready["word"]["id"].as_str().unwrap()).unwrap();
    let first_target_sense_id =
        target_ready["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();

    let mut target_meanings = target_ready["word"]["meanings"].clone();
    let mut second_sense = target_meanings["pos"][0]["senses"][0].clone();
    let second_target_sense_id = Uuid::now_v7();
    second_sense["id"] = json!(second_target_sense_id);
    second_sense["definitions"][0]["id"] = json!(Uuid::now_v7());
    second_sense["definitions"][0]["content_id"] = json!(Uuid::now_v7());
    second_sense["definitions"][0]["content"] = rich_text("保留的第二词义");
    second_sense["sentences"][0]["id"] = json!(Uuid::now_v7());
    second_sense["sentences"][0]["en_text"]["common"]["id"] = json!(Uuid::now_v7());
    second_sense["sentences"][0]["zh_text_id"] = json!(Uuid::now_v7());
    second_sense["sentences"][0]["links"][0]["sense_id"] = json!(second_target_sense_id);
    target_meanings["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .push(second_sense);
    let (status, response) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": 3,
            "intent": "complete",
            "content": target_meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "添加第二词义失败：{response}");
    target_ready = response;
    let (status, target_published) = publish_ready(&state, &bearer, &target_ready).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "目标词发布失败：{target_published}"
    );

    let source_headword = format!("source{}", admin_id.simple());
    let source_ready = create_ready_draft(&state, &pool, &bearer, &source_headword).await;
    let source_entry_id = Uuid::parse_str(source_ready["word"]["id"].as_str().unwrap()).unwrap();
    let mut source_meanings = source_ready["word"]["meanings"].clone();
    let relation_id = Uuid::now_v7();
    source_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id,
        "relation": "synonym",
        "target_word_id": target_entry_id,
        "target_sense_id": first_target_sense_id,
        "target_headword": "客户端伪造词头",
        "target_gloss": "客户端伪造释义",
        "score": "88.50"
    }]);
    source_meanings["pos"][0]["senses"][0]["sentences"][0]["links"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "word_id": target_entry_id,
            "sense_id": second_target_sense_id,
            "role": "context"
        }));
    let (status, source_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": 3,
            "intent": "complete",
            "content": source_meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "关联保存失败：{source_saved}");
    let saved_relation = &source_saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(saved_relation["target_headword"], target_headword);
    assert_eq!(
        saved_relation["target_gloss"],
        format!("{target_headword} 的释义")
    );
    let (status, source_published) = publish_ready(&state, &bearer, &source_saved).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "来源词发布失败：{source_published}"
    );
    let structured_ref_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.entry_publication_sense_refs WHERE entry_id = $1",
    )
    .bind(source_entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(structured_ref_count, 2, "relation/context 应分别结构化锚定");

    let target_revision = target_published["word"]["revision"].as_i64().unwrap();
    let source_revision = source_published["word"]["revision"].as_i64().unwrap();
    let (status, blocked_archive) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": target_revision,
            "base_lifecycle_revision": 1
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "当前入站引用必须阻止单独归档：{blocked_archive}"
    );
    assert_eq!(
        blocked_archive["code"],
        "entry_has_inbound_publication_refs"
    );
    assert_eq!(
        blocked_archive["meta"]["reference_locations"][0]["source_entry_id"],
        source_entry_id.to_string()
    );

    let lifecycle_batch = json!({
        "entries": [
            {
                "id": target_entry_id,
                "base_revision": target_revision,
                "base_lifecycle_revision": 1
            },
            {
                "id": source_entry_id,
                "base_revision": source_revision,
                "base_lifecycle_revision": 1
            }
        ]
    });
    let (status, archived_batch) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/archive-batch"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(lifecycle_batch),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "互相依赖的词条应可原子批量归档：{archived_batch}"
    );
    assert_eq!(archived_batch["affected"], 2);
    assert!(
        archived_batch["words"]
            .as_array()
            .unwrap()
            .iter()
            .all(|word| word["status"] == "archived" && word["lifecycle_revision"] == 2)
    );

    let restore_batch = json!({
        "entries": [
            {
                "id": target_entry_id,
                "base_revision": target_revision,
                "base_lifecycle_revision": 2
            },
            {
                "id": source_entry_id,
                "base_revision": source_revision,
                "base_lifecycle_revision": 2
            }
        ]
    });
    let batch_key = Uuid::now_v7();
    let (status, restored_batch) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/restore-batch"),
        &bearer,
        Some(batch_key),
        Some(restore_batch.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "批量恢复失败：{restored_batch}");
    assert_eq!(restored_batch["affected"], 2);
    let (status, replayed_batch) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/restore-batch"),
        &bearer,
        Some(batch_key),
        Some(restore_batch),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed_batch, restored_batch, "批量命令也必须稳定重放");

    let mut target_without_referenced_sense = target_published["word"]["meanings"].clone();
    let retained_sense = target_without_referenced_sense["pos"][0]["senses"][1].clone();
    target_without_referenced_sense["pos"][0]["senses"] = json!([retained_sense]);
    let (status, target_draft) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": target_published["word"]["revision"],
            "intent": "complete",
            "content": target_without_referenced_sense,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "草稿应允许先删除被引用词义：{target_draft}"
    );
    assert_eq!(target_draft["word"]["has_unpublished_changes"], true);
    assert_eq!(
        target_draft["word"]["published_revision"],
        target_published["word"]["revision"]
    );

    let (status, blocked) = publish_ready(&state, &bearer, &target_draft).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "存在当前入站引用时不得发布删除：{blocked}"
    );
    let inbound_issue = blocked["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "sense_has_inbound_publication_refs")
        .expect("应返回稳定的入站引用问题");
    assert_eq!(inbound_issue["node_id"], first_target_sense_id);
    assert_eq!(
        inbound_issue["reference_location"]["source_entry_id"],
        source_entry_id.to_string()
    );
    assert_eq!(
        inbound_issue["reference_location"]["source_node_id"],
        relation_id.to_string()
    );

    let mut source_without_relation = source_published["word"]["meanings"].clone();
    source_without_relation["pos"][0]["senses"][0]["relations"] = json!([]);
    let (status, source_updated) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source_published["word"]["revision"],
            "intent": "complete",
            "content": source_without_relation,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "移除来源 relation 失败：{source_updated}"
    );
    let (status, source_republished) = publish_ready(&state, &bearer, &source_updated).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "来源词重新发布断开 relation 失败：{source_republished}"
    );

    let (status, target_republished) = publish_ready(&state, &bearer, &target_draft).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "来源当前版本断链后目标应可发布：{target_republished}"
    );
    let historical_relation_refs: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM lexicon.entry_publication_sense_refs
        WHERE entry_id = $1 AND reference_kind = 'relation'
        "#,
    )
    .bind(source_entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        historical_relation_refs, 1,
        "历史 publication 引用应保留可解释性"
    );

    let archive_source = json!({
        "base_revision": source_republished["word"]["revision"],
        "base_lifecycle_revision": 3
    });
    let (status, source_archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{source_entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(archive_source),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "来源归档失败：{source_archived}");
    let (status, target_archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": target_republished["word"]["revision"],
            "base_lifecycle_revision": 3
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "来源已归档后目标应可归档：{target_archived}"
    );

    let (status, unavailable_restore) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{source_entry_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": source_republished["word"]["revision"],
            "base_lifecycle_revision": 4
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "不可用出站目标必须阻止恢复：{unavailable_restore}"
    );
    assert_eq!(
        unavailable_restore["code"],
        "entry_has_unavailable_publication_refs"
    );
}

#[sqlx::test]
async fn concurrent_node_id_reuse_is_serialized_and_returns_validation_error(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let first = create_ready_draft(&state, &pool, &bearer, "collision-one").await;
    let second = create_ready_draft(&state, &pool, &bearer, "collision-two").await;
    let shared_id = Uuid::now_v7();
    let state_ref = &state;
    let bearer_ref = &bearer;

    let request = |word: &Value| {
        let mut content = word["word"]["meanings"].clone();
        content["sense_groups"]
            .as_array_mut()
            .expect("sense_groups 应为数组")
            .push(json!({
                "id": shared_id,
                "name_zh": "并发碰撞",
                "name_en": "Concurrent collision"
            }));
        let entry_id = word["word"]["id"].as_str().unwrap().to_owned();
        let base_revision = word["word"]["revision"].clone();
        async move {
            call(
                state_ref,
                Method::PUT,
                &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
                bearer_ref,
                None,
                Some(json!({
                    "base_revision": base_revision,
                    "intent": "save",
                    "content": content,
                })),
            )
            .await
        }
    };

    let (first_result, second_result) = tokio::join!(request(&first), request(&second));
    let results = [first_result, second_result];
    assert_eq!(
        results
            .iter()
            .filter(|(status, _)| *status == StatusCode::OK)
            .count(),
        1,
        "共享节点 ID 的并发保存只能有一个成功：{results:?}"
    );
    let rejected = results
        .iter()
        .find(|(status, _)| *status == StatusCode::UNPROCESSABLE_ENTITY)
        .expect("另一个并发请求必须返回 422，而不是数据库 500");
    assert!(
        rejected.1["field_issues"]
            .as_array()
            .is_some_and(|issues| issues.iter().any(|issue| issue["code"] == "node_id_reused"))
    );
}

#[sqlx::test]
async fn related_search_reads_only_current_published_snapshots(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let (word_id, sense_id) =
        seed_related_search_entry(&pool, admin_id, "colour", "word", "颜色", true, false).await;
    let (second_word_id, second_sense_id) =
        seed_related_search_entry(&pool, admin_id, "colour", "word", "色彩", true, false).await;
    seed_related_search_entry(
        &pool,
        admin_id,
        "colour draft",
        "word",
        "未发布",
        false,
        false,
    )
    .await;
    seed_related_search_entry(
        &pool,
        admin_id,
        "colour archived",
        "word",
        "已归档",
        true,
        true,
    )
    .await;
    let (phrase_id, phrase_sense_id) = seed_related_search_entry(
        &pool,
        admin_id,
        "colour centre",
        "phrase",
        "色彩中心",
        true,
        false,
    )
    .await;
    let (unicode_id, _) =
        seed_related_search_entry(&pool, admin_id, "İ", "word", "带点大写 I", true, false).await;

    let (status, empty) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=%20%20%20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty, json!({"results": []}));

    let (status, empty_v2) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=%20%20%20&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        empty_v2,
        json!({"results": [], "total": 0, "next_cursor": null})
    );

    let (status, words) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=%20COLO%20&kind=word&limit=10"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "关联词搜索失败：{words}");
    assert_eq!(
        words,
        json!({
            "results": [{
                "word_id": word_id,
                "headword": "colour",
                "kind": "word",
                "dialects": ["common"],
                "pos_labels": [],
                "senses": [{"sense_id": sense_id, "gloss": "颜色"}]
            }, {
                "word_id": second_word_id,
                "headword": "colour",
                "kind": "word",
                "dialects": ["common"],
                "pos_labels": [],
                "senses": [{"sense_id": second_sense_id, "gloss": "色彩"}]
            }]
        })
    );

    let (status, phrases) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=colour&kind=phrase"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "短语筛选失败：{phrases}");
    assert_eq!(
        phrases,
        json!({
            "results": [{
                "word_id": phrase_id,
                "headword": "colour centre",
                "kind": "phrase",
                "dialects": ["common"],
                "pos_labels": [],
                "senses": [{"sense_id": phrase_sense_id, "gloss": "色彩中心"}]
            }]
        })
    );

    let (status, exact_first) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=colour&kind=word&match_mode=exact&page_size=1"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "exact 首页失败：{exact_first}");
    assert_eq!(exact_first["total"], 2);
    assert_eq!(exact_first["results"].as_array().unwrap().len(), 1);
    assert!(exact_first.get("next_cursor").is_some());
    let cursor = exact_first["next_cursor"]
        .as_str()
        .expect("应有第二页 cursor");
    let mut tampered_cursor = cursor.as_bytes().to_vec();
    let last = tampered_cursor.last_mut().unwrap();
    *last = if *last == b'A' { b'B' } else { b'A' };
    let tampered_cursor = String::from_utf8(tampered_cursor).unwrap();
    let (status, invalid_cursor) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=colour&kind=word&match_mode=exact&page_size=1&cursor={tampered_cursor}"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid_cursor["field"], "cursor");

    let second_admin_id = seed_admin(&pool).await;
    let second_bearer = token(&state, second_admin_id);
    for (uri, request_bearer) in [
        (
            format!(
                "{ROOT}/entries/related-search?q=colours&kind=word&match_mode=exact&page_size=1&cursor={cursor}"
            ),
            bearer.as_str(),
        ),
        (
            format!(
                "{ROOT}/entries/related-search?q=colour&kind=phrase&match_mode=exact&page_size=1&cursor={cursor}"
            ),
            bearer.as_str(),
        ),
        (
            format!(
                "{ROOT}/entries/related-search?q=colour&kind=word&match_mode=exact&page_size=1&cursor={cursor}"
            ),
            second_bearer.as_str(),
        ),
    ] {
        let (status, mismatched_cursor) =
            call(&state, Method::GET, &uri, request_bearer, None, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(mismatched_cursor["field"], "cursor");
    }

    let first_page_id =
        Uuid::parse_str(exact_first["results"][0]["word_id"].as_str().unwrap()).unwrap();
    let unread_id = if first_page_id == word_id {
        second_word_id
    } else {
        word_id
    };
    sqlx::query("UPDATE lexicon.entries SET updated_at = now() WHERE id = $1")
        .bind(unread_id)
        .execute(&pool)
        .await
        .unwrap();

    let (status, exact_second) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=colour&kind=word&match_mode=exact&page_size=1&cursor={cursor}"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "exact 第二页失败：{exact_second}");
    assert_eq!(exact_second["total"], 2);
    assert!(exact_second.get("next_cursor").is_some());
    assert!(exact_second["next_cursor"].is_null());
    let first_id = exact_first["results"][0]["word_id"].as_str().unwrap();
    let second_id = exact_second["results"][0]["word_id"].as_str().unwrap();
    assert_ne!(first_id, second_id, "同名词条不得合并或重复");

    let (status, exact_second_replay) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=colour&kind=word&match_mode=exact&page_size=1&cursor={cursor}"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        exact_second_replay, exact_second,
        "同一 cursor 必须稳定重放"
    );

    let (status, normalized_exact) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=%EF%BC%A3%EF%BC%AF%EF%BC%AC%EF%BC%AF%EF%BC%B5%EF%BC%B2&kind=word&match_mode=exact&page_size=20"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "NFKC exact 搜索失败：{normalized_exact}"
    );
    assert_eq!(normalized_exact["total"], 2);
    assert_eq!(normalized_exact["results"].as_array().unwrap().len(), 2);
    assert!(normalized_exact.get("next_cursor").is_some());
    assert!(normalized_exact["next_cursor"].is_null());

    let (status, unicode_exact) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=%C4%B0&kind=word&match_mode=exact&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "Unicode exact 搜索失败：{unicode_exact}"
    );
    assert_eq!(unicode_exact["total"], 1);
    assert_eq!(
        unicode_exact["results"][0]["word_id"],
        unicode_id.to_string()
    );

    let (status, contains) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=colour&match_mode=contains&exclude_exact=true&page_size=20"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "contains 搜索失败：{contains}");
    assert_eq!(contains["total"], 1);
    assert_eq!(contains["results"][0]["word_id"], phrase_id.to_string());

    let changing = create_ready_draft(&state, &pool, &bearer, "pagination-change").await;
    let (status, published_change) = publish_ready(&state, &bearer, &changing).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "发布并发目标失败：{published_change}"
    );
    let (status, changed_dataset_cursor) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=colour&kind=word&match_mode=exact&page_size=1&cursor={cursor}"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(changed_dataset_cursor["field"], "cursor");

    let (status, before_archive) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=colour&kind=word&match_mode=exact&page_size=1"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let before_archive_cursor = before_archive["next_cursor"].as_str().unwrap();
    let changing_id = published_change["word"]["id"].as_str().unwrap();
    let (status, archived_change) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{changing_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": published_change["word"]["revision"],
            "base_lifecycle_revision": published_change["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "归档并发目标失败：{archived_change}"
    );
    let (status, archived_dataset_cursor) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=colour&kind=word&match_mode=exact&page_size=1&cursor={before_archive_cursor}"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(archived_dataset_cursor["field"], "cursor");

    let (status, before_restore) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=colour&kind=word&match_mode=exact&page_size=1"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let before_restore_cursor = before_restore["next_cursor"].as_str().unwrap();
    let (status, restored_change) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{changing_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": archived_change["word"]["revision"],
            "base_lifecycle_revision": archived_change["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "恢复并发目标失败：{restored_change}"
    );
    let (status, restored_dataset_cursor) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=colour&kind=word&match_mode=exact&page_size=1&cursor={before_restore_cursor}"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(restored_dataset_cursor["field"], "cursor");

    let (status, invalid) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=colour&page_size=1&limit=1"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid["field"], "page_size");

    for limit in [0, 101] {
        let (status, invalid) = call(
            &state,
            Method::GET,
            &format!("{ROOT}/entries/related-search?q=colour&limit={limit}"),
            &bearer,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(invalid["code"], "invalid_query");
        assert_eq!(invalid["field"], "limit");
    }
}

#[sqlx::test]
async fn nul_text_is_rejected_as_query_or_draft_validation(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    for (uri, field) in [
        (format!("{ROOT}/entries?q=%00"), "q"),
        (format!("{ROOT}/entries?gloss=%00"), "gloss"),
        (format!("{ROOT}/entries?pos=%00"), "pos"),
        (format!("{ROOT}/entries/related-search?q=%00"), "q"),
    ] {
        let (status, invalid) = call(&state, Method::GET, &uri, &bearer, None, None).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "NUL 查询应返回 400：{invalid}"
        );
        assert_eq!(invalid["code"], "invalid_query");
        assert_eq!(invalid["field"], field);
    }

    let ready = create_ready_draft(&state, &pool, &bearer, "nul-guard").await;
    let entry_id = ready["word"]["id"].as_str().unwrap();
    let revision = ready["word"]["revision"].as_i64().unwrap();

    let mut forms = ready["word"]["forms"].clone();
    forms["pos"][0]["base_form"]["variants"][0]["pronunciations"][0]["actual_pron"] =
        json!("unsafe\0pronunciation");
    let (status, invalid_forms) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "content": forms,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "forms NUL 应在写数据库前返回 422：{invalid_forms}"
    );
    assert!(
        invalid_forms["field_issues"]
            .as_array()
            .is_some_and(|issues| {
                issues
                    .iter()
                    .any(|issue| issue["code"] == "nul_character_not_allowed")
            })
    );

    let mut meanings = ready["word"]["meanings"].clone();
    meanings["sense_groups"][0]["name_zh"] = json!("unsafe\0name");
    meanings["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] =
        json!("unsafe\0definition");
    let (status, invalid_meanings) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "meanings/RichText NUL 应在写数据库前返回 422：{invalid_meanings}"
    );
    assert!(
        invalid_meanings["field_issues"]
            .as_array()
            .is_some_and(|issues| {
                issues
                    .iter()
                    .any(|issue| issue["code"] == "nul_character_not_allowed")
            })
    );
}

#[sqlx::test]
async fn phrase_detection_creation_editing_and_publication_use_the_v2_aggregate(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let phrase = format!("look up {}", admin_id.simple());
    seed_dictionary_term(&pool, &phrase, "phrase", "common_unmarked").await;

    let ready = create_ready_draft(&state, &pool, &bearer, &phrase).await;
    assert_eq!(ready["word"]["kind"], "phrase");
    assert_eq!(ready["word"]["detection_snapshot"]["entry_kind"], "phrase");
    assert_eq!(ready["word"]["revision"], 3);
    assert_eq!(ready["word"]["max_reachable_step"], "preview");

    let (status, published) = publish_ready(&state, &bearer, &ready).await;
    assert_eq!(status, StatusCode::CREATED, "短语发布失败：{published}");
    assert_eq!(published["word"]["kind"], "phrase");
    assert_eq!(published["word"]["status"], "published");
    assert_eq!(published["word"]["published_revision"], 3);

    let missing = format!("unlisted phrase {}", Uuid::now_v7().simple());
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": missing})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "未收录短语检测失败：{detection}");
    assert_eq!(detection["entry_kind"], "phrase");
    assert_eq!(detection["builtin_dictionary"]["status"], "not_found");
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {"mode": "unified", "common": missing},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "未收录短语建稿失败：{created}");
    assert_eq!(created["word"]["kind"], "phrase");
    assert_eq!(created["word"]["forms"], json!({"pos": []}));
}

#[sqlx::test]
async fn lifecycle_commands_preserve_publications_and_are_idempotent_under_double_click(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let ready = create_ready_draft(&state, &pool, &bearer, "archive-safe").await;
    let (status, published) = publish_ready(&state, &bearer, &ready).await;
    assert_eq!(status, StatusCode::CREATED, "发布失败：{published}");
    let entry_id = published["word"]["id"].as_str().unwrap();
    let entry_uuid = Uuid::parse_str(entry_id).unwrap();
    let revision = published["word"]["revision"].as_i64().unwrap();
    let publication_before: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();

    let body = json!({"base_revision": revision, "base_lifecycle_revision": 1});
    let (status, missing_header) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/archive"),
        &bearer,
        None,
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "缺少幂等头：{missing_header}"
    );

    let key = Uuid::now_v7();
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/archive"),
        &bearer,
        Some(key),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "归档失败：{archived}");
    assert_eq!(archived["word"]["status"], "archived");
    assert_eq!(archived["word"]["revision"], revision);
    assert_eq!(archived["word"]["lifecycle_revision"], 2);
    assert_eq!(archived["word"]["published_revision"], revision);
    assert!(archived["word"]["archived_at"].is_string());
    assert_eq!(archived["word"]["archived_by"], admin_id.to_string());

    let (status, default_list) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?q=archive-safe"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "默认列表查询失败：{default_list}");
    assert_eq!(default_list["words"], json!([]));

    let (status, archived_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "archive-safe"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "归档词条检测失败：{archived_detection}"
    );
    assert_eq!(archived_detection["smart_dictionary"]["status"], "warning");
    assert!(
        archived_detection["smart_dictionary"]["duplicates"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert!(
        archived_detection["smart_dictionary"]["surface_match_page"]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| {
                item["existing"]["word_id"] == json!(entry_id)
                    && item["existing"]["status"] == "archived"
                    && item["can_continue"] == true
            })),
        "归档词条必须作为可确认 warning 返回并明确显示 archived 状态"
    );

    let (status, replayed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/archive"),
        &bearer,
        Some(key),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed, archived, "同幂等键必须逐字段重放原响应");

    let (status, conflict) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/archive"),
        &bearer,
        Some(key),
        Some(json!({"base_revision": revision, "base_lifecycle_revision": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "idempotency_conflict");

    let (status, editing_archived) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "content": archived["word"]["meanings"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(editing_archived["code"], "entry_archived");

    let publication_after: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    let publication_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publications WHERE entry_id = $1")
            .bind(entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(publication_after, publication_before);
    assert_eq!(publication_count, 1, "归档不得删除或复制 publication");

    let (status, restored) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({"base_revision": revision, "base_lifecycle_revision": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "恢复失败：{restored}");
    assert_eq!(restored["word"]["status"], "published");
    assert_eq!(restored["word"]["lifecycle_revision"], 3);
    assert!(restored["word"].get("archived_at").is_none());

    let archive_uri = format!("{ROOT}/entries/{entry_id}/archive");
    let double_click_body = json!({"base_revision": revision, "base_lifecycle_revision": 3});
    let (first, second) = tokio::join!(
        call(
            &state,
            Method::POST,
            &archive_uri,
            &bearer,
            Some(Uuid::now_v7()),
            Some(double_click_body.clone()),
        ),
        call(
            &state,
            Method::POST,
            &archive_uri,
            &bearer,
            Some(Uuid::now_v7()),
            Some(double_click_body),
        )
    );
    assert_eq!(first.0, StatusCode::OK, "首次双击归档失败：{}", first.1);
    assert_eq!(second.0, StatusCode::OK, "第二次双击归档失败：{}", second.1);
    let lifecycle_revision: i64 =
        sqlx::query_scalar("SELECT lifecycle_revision FROM lexicon.entries WHERE id = $1")
            .bind(entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(lifecycle_revision, 4, "并发双击只能产生一次状态迁移");

    let (status, invalid) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({"base_revision": 0, "base_lifecycle_revision": 0})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(invalid["code"], "validation_failed");

    let lifecycle_uri = format!("{ROOT}/entries/{entry_id}/restore");
    let malformed_json_key = Uuid::now_v7().to_string();
    let invalid_path_key = Uuid::now_v7().to_string();
    for (label, uri, key, raw_body) in [
        (
            "非法幂等头",
            lifecycle_uri.as_str(),
            "not-a-uuid",
            br#"{"base_revision":1,"base_lifecycle_revision":1}"#.as_slice(),
        ),
        (
            "非法 JSON",
            lifecycle_uri.as_str(),
            malformed_json_key.as_str(),
            br#"{"base_revision":"#.as_slice(),
        ),
        (
            "非法路径 UUID",
            "/api/v1/admin/lexicon/entries/not-a-uuid/restore",
            invalid_path_key.as_str(),
            br#"{"base_revision":1,"base_lifecycle_revision":1}"#.as_slice(),
        ),
    ] {
        let (status, problem) =
            call_raw(&state, Method::POST, uri, &bearer, Some(key), raw_body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}：{problem}");
        assert_eq!(problem["status"], 400, "{label} 应返回 Problem Details");
        assert!(problem["code"].is_string(), "{label} 应返回稳定错误码");
    }

    let duplicate = json!({
        "entries": [
            {"id": entry_id, "base_revision": revision, "base_lifecycle_revision": 4},
            {"id": entry_id, "base_revision": revision, "base_lifecycle_revision": 4}
        ]
    });
    let too_many = (0..101)
        .map(|_| {
            json!({
                "id": Uuid::now_v7(),
                "base_revision": 1,
                "base_lifecycle_revision": 1
            })
        })
        .collect::<Vec<_>>();
    for body in [
        json!({"entries": []}),
        duplicate,
        json!({"entries": too_many}),
    ] {
        let (status, problem) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/entries/restore-batch"),
            &bearer,
            Some(Uuid::now_v7()),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
        assert_eq!(problem["code"], "validation_failed");
    }

    let second = create_ready_draft(&state, &pool, &bearer, "atomic-stale").await;
    let second_id = second["word"]["id"].as_str().unwrap();
    let second_revision = second["word"]["revision"].as_i64().unwrap();
    let (status, stale_batch) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/restore-batch"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "entries": [
                {
                    "id": entry_id,
                    "base_revision": revision,
                    "base_lifecycle_revision": 4
                },
                {
                    "id": second_id,
                    "base_revision": second_revision + 1,
                    "base_lifecycle_revision": 1
                }
            ]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale_batch}");
    assert_eq!(stale_batch["code"], "revision_conflict");
    let lifecycle_after_failed_batch: (i64, bool) = sqlx::query_as(
        "SELECT lifecycle_revision, archived_at IS NOT NULL FROM lexicon.entries WHERE id = $1",
    )
    .bind(entry_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        lifecycle_after_failed_batch,
        (4, true),
        "批量中的任意 revision 冲突必须回滚所有生命周期迁移"
    );
}

#[sqlx::test]
async fn dialect_suggestions_use_dictionary_region_evidence_and_preserve_rich_text_offsets(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    seed_dictionary_term(&pool, "colour", "word", "british_core").await;
    seed_dictionary_term(&pool, "color", "word", "american_core").await;
    let dataset_id: i64 =
        sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status = 'active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.region_surfaces (
            dataset_id, normalized_term, term, region_family, families,
            source_regions, evidence_types, pos, targets, is_headword
        ) VALUES (
            $1, 'colour', 'colour', 'british_core', ARRAY['british_core'],
            ARRAY['GB'], ARRAY['spelling'], ARRAY['noun'], ARRAY['color'], true
        )
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();

    let (status, response) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/dialect-variant-suggestions"),
        &bearer,
        None,
        Some(json!({
            "source_dialect": "uk",
            "target_dialect": "us",
            "items": [
                {"client_id": "form", "field_kind": "form", "value": "COLOUR"},
                {
                    "client_id": "definition",
                    "field_kind": "definition",
                    "value": {
                        "version": 2,
                        "text": "Colour is vivid",
                        "annotations": [{"type": "emphasis", "start": 0, "end": 6, "level": "strong"}]
                    }
                }
            ]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "方言建议失败：{response}");
    assert_eq!(response["provider"]["kind"], "dictionary_region_rules");
    assert_eq!(response["provider"]["version"], "1");
    assert_eq!(response["suggestions"][0]["value"], "COLOR");
    assert_eq!(
        response["suggestions"][1]["value"]["text"],
        "Color is vivid"
    );
    assert_eq!(
        response["suggestions"][1]["value"]["annotations"][0]["end"],
        5
    );

    let (status, invalid) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/dialect-variant-suggestions"),
        &bearer,
        None,
        Some(json!({
            "source_dialect": "uk",
            "target_dialect": "uk",
            "items": [{"client_id": "form", "field_kind": "form", "value": "colour"}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(invalid["code"], "validation_failed");
    assert_eq!(invalid["meta"]["code"], "target_dialect");
}

#[sqlx::test]
async fn detection_distinguishes_center_and_centre_in_both_directions(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    seed_dictionary_term(&pool, "center", "word", "british_american").await;
    seed_dictionary_term(&pool, "centre", "word", "british_core").await;
    let dataset_id: i64 =
        sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status = 'active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.region_surfaces (
            dataset_id, normalized_term, term, region_family, families,
            source_regions, evidence_types, pos, targets, is_headword
        ) VALUES
            ($1, 'center', 'center', 'british_american', ARRAY['british_core', 'american_core'],
             ARRAY['GB', 'US'], ARRAY['spelling'], ARRAY['noun'], ARRAY['centre'], true),
            ($1, 'centre', 'centre', 'british_core', ARRAY['british_core'],
             ARRAY['GB'], ARRAY['spelling'], ARRAY['noun'], ARRAY['center'], true)
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();

    for (input, source_dialect) in [("center", "us"), ("centre", "uk")] {
        let (status, response) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/detections"),
            &bearer,
            None,
            Some(json!({"language": "en", "headword": input})),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{input} 检测失败：{response}");
        assert_eq!(
            response["builtin_dictionary"]["headwords"],
            json!({
                "mode": "distinguish",
                "uk": "centre",
                "us": "center",
                "source_dialect": source_dialect
            })
        );
    }

    seed_dictionary_term(&pool, "priority-source", "word", "british_core").await;
    seed_dictionary_term(&pool, "priority-ambiguous", "word", "british_american").await;
    seed_dictionary_term(&pool, "priority-us", "word", "american_core").await;
    sqlx::query(
        r#"
        INSERT INTO dictionary.region_surfaces (
            dataset_id, normalized_term, term, region_family, families,
            source_regions, evidence_types, pos, targets, is_headword
        ) VALUES (
            $1, 'priority-source', 'priority-source', 'british_core', ARRAY['british_core'],
            ARRAY['GB'], ARRAY['spelling'], ARRAY['noun'],
            ARRAY['priority-ambiguous', 'priority-us'], true
        )
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();
    let (status, response) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "priority-source"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "候选优先级检测失败：{response}");
    assert_eq!(
        response["builtin_dictionary"]["headwords"],
        json!({
            "mode": "distinguish",
            "uk": "priority-source",
            "us": "priority-us",
            "source_dialect": "uk"
        }),
        "明确反方言候选必须优先于排列在前的混合候选"
    );
}

#[sqlx::test]
async fn matched_dictionary_headwords_are_editable_suggestions(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    seed_dictionary_term(&pool, "manual-common", "word", "common_unmarked").await;
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "manual-common"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "统一词形检测失败：{detection}");
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {
                "mode": "distinguish",
                "uk": "  manual-edited-uk  ",
                "us": " manual-common ",
                "source_dialect": "us"
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "统一改区分应成功：{created}");
    assert_eq!(created["word"]["headwords"]["mode"], "distinguish");
    assert_eq!(created["word"]["headwords"]["uk"], "manual-edited-uk");
    assert_eq!(created["word"]["headwords"]["us"], "manual-common");
    assert_eq!(
        created["word"]["forms"]["pos"][0]["dialect_rules"],
        json!({"spelling_mode": "distinguish", "phonetic_mode": "distinguish"}),
        "统一改区分后 Step 2 的方言规则必须同步"
    );
    assert_eq!(
        created["word"]["forms"]["pos"][0]["base_form"]["variants"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "统一改区分后必须生成英美两侧基础词形"
    );
    assert_eq!(
        created["word"]["forms"]["pos"][0]["base_form"]["variants"][0]["origin"], "manual",
        "用户修改的英式词形必须标记为手工来源"
    );
    assert_eq!(
        created["word"]["forms"]["pos"][0]["base_form"]["variants"][1]["origin"], "dictionary",
        "保持检测基准的美式词形必须保留词典来源"
    );
    let created_id = Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap();
    let persisted_origins: Vec<(String, String)> = sqlx::query_as(
        "SELECT dialect, origin FROM lexicon.entry_headwords WHERE entry_id = $1 ORDER BY dialect",
    )
    .bind(created_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        persisted_origins,
        vec![
            ("uk".to_owned(), "manual".to_owned()),
            ("us".to_owned(), "dictionary".to_owned()),
        ],
        "持久化词头必须逐侧保留真实来源"
    );

    seed_dictionary_term(&pool, "manual-uk", "word", "british_core").await;
    seed_dictionary_term(&pool, "manual-us", "word", "american_core").await;
    let dataset_id: i64 =
        sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status = 'active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.region_surfaces (
            dataset_id, normalized_term, term, region_family, families,
            source_regions, evidence_types, pos, targets, is_headword
        ) VALUES
            ($1, 'manual-uk', 'manual-uk', 'british_core', ARRAY['british_core'],
             ARRAY['GB'], ARRAY['spelling'], ARRAY['noun'], ARRAY['manual-us'], true),
            ($1, 'manual-us', 'manual-us', 'american_core', ARRAY['american_core'],
             ARRAY['US'], ARRAY['spelling'], ARRAY['noun'], ARRAY['manual-uk'], true)
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "manual-us"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "区分词形检测失败：{detection}");
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {"mode": "unified", "common": "manual-us"}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "区分改统一应成功：{created}");
    assert_eq!(created["word"]["headwords"]["mode"], "unified");
    assert_eq!(
        created["word"]["forms"]["pos"][0]["dialect_rules"],
        json!({"spelling_mode": "unified", "phonetic_mode": "unified"}),
        "区分改统一后 Step 2 的方言规则必须同步"
    );
    assert_eq!(
        created["word"]["forms"]["pos"][0]["base_form"]["variants"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "区分改统一后必须只保留共同基础词形"
    );

    seed_dictionary_term(&pool, "tamper-common", "word", "common_unmarked").await;
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "tamper-common"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "篡改测试检测失败：{detection}");
    let (status, rejected) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {
                "mode": "distinguish",
                "uk": "manual-edited-uk",
                "us": "tampered-source",
                "source_dialect": "us"
            }
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "命中侧被篡改时必须拒绝：{rejected}"
    );
    assert_eq!(rejected["code"], "detection_mismatch");

    seed_dictionary_term(&pool, "single-uk", "word", "british_core").await;
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "single-uk"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "单侧英式检测失败：{detection}");
    assert_eq!(
        detection["builtin_dictionary"]["headwords"]["mode"],
        "unified"
    );
    assert_eq!(detection["matched_dialect"], "uk");

    let (status, rejected) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {
                "mode": "distinguish",
                "uk": "single-uk-edited",
                "us": "single-uk",
                "source_dialect": "us"
            }
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "单侧英式命中不得伪装为美式来源：{rejected}"
    );
    assert_eq!(rejected["code"], "detection_mismatch");

    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {
                "mode": "distinguish",
                "uk": "single-uk",
                "us": "single-us-edited",
                "source_dialect": "uk"
            }
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "保持英式命中侧应允许创建：{created}"
    );
}

#[sqlx::test]
async fn never_published_draft_can_be_deleted_but_published_entry_is_protected(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let draft = create_ready_draft(&state, &pool, &bearer, "deletable-draft").await;
    let draft_id = draft["word"]["id"].as_str().unwrap();
    let draft_revision = draft["word"]["revision"].as_i64().unwrap();
    let draft_lifecycle_revision = draft["word"]["lifecycle_revision"].as_i64().unwrap();
    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{draft_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": 0,
            "base_lifecycle_revision": draft_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "非法 revision 必须返回 422：{response}"
    );
    assert_eq!(response["meta"]["code"], "base_revision");

    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{draft_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": draft_revision,
            "base_lifecycle_revision": 0
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "非法 lifecycle revision 必须返回 422：{response}"
    );
    assert_eq!(response["meta"]["code"], "base_lifecycle_revision");

    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{draft_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": draft_revision - 1,
            "base_lifecycle_revision": draft_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "过期 revision 不得删除较新的草稿：{response}"
    );
    assert_eq!(response["code"], "revision_conflict");
    assert_eq!(response["meta"]["current_revision"], draft_revision);

    let archive_race = create_ready_draft(&state, &pool, &bearer, "archive-delete-race").await;
    let archive_race_id = archive_race["word"]["id"].as_str().unwrap();
    let archive_race_revision = archive_race["word"]["revision"].as_i64().unwrap();
    let archive_race_lifecycle_revision =
        archive_race["word"]["lifecycle_revision"].as_i64().unwrap();
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{archive_race_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": archive_race_revision,
            "base_lifecycle_revision": archive_race_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "并发测试归档失败：{archived}");
    let archived_lifecycle_revision = archived["word"]["lifecycle_revision"].as_i64().unwrap();

    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{archive_race_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": archive_race_revision,
            "base_lifecycle_revision": archive_race_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "旧页面不得删除刚被归档的草稿：{response}"
    );
    assert_eq!(response["code"], "revision_conflict");
    assert_eq!(
        response["meta"]["current_lifecycle_revision"],
        archived_lifecycle_revision
    );

    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{archive_race_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": archive_race_revision,
            "base_lifecycle_revision": archived_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "归档状态不是可永久删除的草稿：{response}"
    );
    assert_eq!(response["code"], "entry_not_deletable");

    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{draft_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": draft_revision,
            "base_lifecycle_revision": draft_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "删除草稿失败：{response}");
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM lexicon.entries WHERE id = $1)")
            .bind(Uuid::parse_str(draft_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!exists, "删除必须级联清理草稿聚合并释放词头");

    let draft_tombstones: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*) FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_scope = 'draft'
          AND is_deleted = TRUE AND source_revision = $2
        "#,
    )
    .bind(Uuid::parse_str(draft_id).unwrap())
    .bind(draft_revision + 1)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        draft_tombstones > 0,
        "删除必须保留更高 source revision 的 projection tombstone"
    );
    let consumed_detection_id = Uuid::parse_str(
        draft["word"]["detection_snapshot"]["detection_id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let consumed_entry_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT entry_id FROM lexicon.consumed_detections WHERE actor_id = $1 AND detection_id = $2",
    )
    .bind(admin_id)
    .bind(consumed_detection_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        consumed_entry_id, None,
        "删除草稿后必须保留独立的 detection 消费墓碑"
    );
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit.admin_actions WHERE action = 'lexicon.entry.delete_draft' AND resource_id = $1",
    )
    .bind(Uuid::parse_str(draft_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audit_count, 1, "永久删除草稿必须留下管理员审计记录");

    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{draft_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": draft_revision,
            "base_lifecycle_revision": draft_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "重复删除应返回 404：{response}"
    );

    let referenced = create_ready_draft(&state, &pool, &bearer, "referenced-draft").await;
    let referring = create_ready_draft(&state, &pool, &bearer, "referring-draft").await;
    let referenced_id = Uuid::parse_str(referenced["word"]["id"].as_str().unwrap()).unwrap();
    let referenced_revision = referenced["word"]["revision"].as_i64().unwrap();
    let referenced_lifecycle_revision = referenced["word"]["lifecycle_revision"].as_i64().unwrap();
    let referenced_sense_id = Uuid::parse_str(
        referenced["word"]["meanings"]["pos"][0]["senses"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let referring_id = Uuid::parse_str(referring["word"]["id"].as_str().unwrap()).unwrap();
    let referring_sense_id = Uuid::parse_str(
        referring["word"]["meanings"]["pos"][0]["senses"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let relation_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.nodes (
            id, entry_id, node_type, parent_node_id, node_role, stable_slot
        ) VALUES ($1, $2, 'relation', $3, 'meanings.relation', false)
        "#,
    )
    .bind(relation_id)
    .bind(referring_id)
    .bind(referring_sense_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.relations (
            id, entry_id, source_sense_id, relation_type,
            target_entry_id, target_sense_id, score,
            target_headword_snapshot, target_gloss_snapshot, sort_order
        ) VALUES ($1, $2, $3, 'synonym', $4, $5, 100, 'referenced-draft', '', 0)
        "#,
    )
    .bind(relation_id)
    .bind(referring_id)
    .bind(referring_sense_id)
    .bind(referenced_id)
    .bind(referenced_sense_id)
    .execute(&pool)
    .await
    .unwrap();
    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{referenced_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": referenced_revision,
            "base_lifecycle_revision": referenced_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "被其他草稿引用的词条不得删除：{response}"
    );
    assert_eq!(response["code"], "entry_not_deletable");

    let published = create_ready_draft(&state, &pool, &bearer, "protected-published").await;
    let (status, published) = publish_ready(&state, &bearer, &published).await;
    assert_eq!(status, StatusCode::CREATED, "发布准备失败：{published}");
    let published_id = published["word"]["id"].as_str().unwrap();
    let published_revision = published["word"]["revision"].as_i64().unwrap();
    let published_lifecycle_revision = published["word"]["lifecycle_revision"].as_i64().unwrap();
    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{published_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": published_revision,
            "base_lifecycle_revision": published_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "已发布词条不得删除：{response}"
    );
    assert_eq!(response["code"], "entry_not_deletable");
}

#[sqlx::test]
async fn forms_impact_is_complete_stable_and_detects_pos_code_replacement(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target = create_ready_draft(&state, &pool, &bearer, "impact-target").await;
    let (status, target) = publish_ready(&state, &bearer, &target).await;
    assert_eq!(status, StatusCode::CREATED, "目标词发布失败：{target}");
    let target_entry_id = target["word"]["id"].clone();
    let target_sense_id = target["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();

    let source = create_ready_draft(&state, &pool, &bearer, "impact-source").await;
    let entry_id = source["word"]["id"].as_str().unwrap().to_owned();
    let mut meanings = source["word"]["meanings"].clone();
    let relation_id = Uuid::now_v7();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id,
        "relation": "synonym",
        "target_word_id": target_entry_id,
        "target_sense_id": target_sense_id,
        "score": "75"
    }]);
    let (status, source) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "relation 保存失败：{source}");
    let revision = source["word"]["revision"].as_i64().unwrap();
    let forms = source["word"]["forms"].clone();

    let (status, unchanged) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({"base_revision": revision, "content": forms})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "无变化 impact 失败：{unchanged}");
    assert_eq!(unchanged["requires_confirmation"], false);
    assert_eq!(unchanged["affected"], json!([]));
    assert!(unchanged["confirmation_token"].is_null());

    let pos = &source["word"]["meanings"]["pos"][0];
    let sense = &pos["senses"][0];
    let definition = &sense["definitions"][0];
    let sentence = &sense["sentences"][0];
    let expected_ids = vec![
        source["word"]["forms"]["pos"][0]["pos_id"].clone(),
        pos["grammar_structures"][0]["id"].clone(),
        pos["grammar_structures"][0]["variants"][0]["id"].clone(),
        sense["id"].clone(),
        definition["id"].clone(),
        definition["content_id"].clone(),
        sentence["id"].clone(),
        sentence["en_text"]["common"]["id"].clone(),
        sentence["zh_text_id"].clone(),
        json!(relation_id),
    ];
    let expected_types = vec![
        "pos",
        "grammar_structure",
        "text_variant",
        "sense",
        "definition",
        "text_variant",
        "sentence",
        "text_variant",
        "text_variant",
        "relation",
    ];

    let mut deleted_forms = forms.clone();
    deleted_forms["pos"] = json!([]);
    let impact_uri = format!("{ROOT}/entries/{entry_id}/steps/forms/impact");
    let preview_deleted = || {
        let content = deleted_forms.clone();
        call(
            &state,
            Method::POST,
            &impact_uri,
            &bearer,
            None,
            Some(json!({
                "base_revision": revision,
                "content": content,
            })),
        )
    };
    let (status, deleted) = preview_deleted().await;
    assert_eq!(status, StatusCode::OK, "删除 POS impact 失败：{deleted}");
    assert_eq!(deleted["requires_confirmation"], true);
    let deleted_items = deleted["affected"].as_array().unwrap();
    assert_eq!(
        deleted_items
            .iter()
            .map(|item| item["node_id"].clone())
            .collect::<Vec<_>>(),
        expected_ids
    );
    assert_eq!(
        deleted_items
            .iter()
            .map(|item| item["node_type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected_types
    );
    assert_eq!(
        deleted_items
            .iter()
            .map(|item| item["node_id"].clone())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        deleted_items.len(),
        "affected 中 node_id 不得重复"
    );

    let (status, deleted_again) = preview_deleted().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        deleted_again["affected"], deleted["affected"],
        "同 revision、同请求的 impact diff 必须稳定"
    );

    let mut replaced_forms = forms;
    replaced_forms["pos"][0]["pos"] = json!("adjective");
    let (status, invalid_replacement) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "content": replaced_forms,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        invalid_replacement["field_issues"][0]["code"],
        "invalid_form_type_for_part_of_speech"
    );
    assert_eq!(
        invalid_replacement["field_issues"][0]["node_id"],
        replaced_forms["pos"][0]["form_groups"][0]["slots"][0]["id"]
    );
    replaced_forms["pos"][0]["form_groups"][0]["slots"][0]["form_type"] = json!("comparative");
    replaced_forms["pos"][0]["form_groups"][0]["slots"][0]["id"] = json!(Uuid::now_v7());
    replaced_forms["pos"][0]["form_groups"][0]["slots"][0]["variants"][0]["id"] =
        json!(Uuid::now_v7());
    replaced_forms["pos"][0]["form_groups"][0]["slots"][0]["variants"][0]["pronunciations"][0]["id"] =
        json!(Uuid::now_v7());
    let (status, replaced) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "content": replaced_forms,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "替换 POS code impact 失败：{replaced}"
    );
    assert_eq!(replaced["requires_confirmation"], true);
    assert_eq!(
        replaced["affected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["node_id"].clone())
            .collect::<Vec<_>>(),
        expected_ids
    );

    let (status, confirmation_required) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "content": replaced_forms,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "未确认应拒绝：{confirmation_required}"
    );
    assert_eq!(
        confirmation_required["code"],
        "downstream_confirmation_required"
    );
    assert_eq!(
        confirmation_required["meta"]["affected_node_ids"],
        json!(expected_ids)
    );

    let old_sense_id = sense["id"].clone();
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "confirmed_impact_token": replaced["confirmation_token"],
            "content": replaced_forms,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "确认后保存失败：{saved}");
    assert_eq!(saved["word"]["forms"]["pos"][0]["pos"], "adjective");
    assert_eq!(
        saved["word"]["meanings"]["pos"][0]["pos_id"],
        saved["word"]["forms"]["pos"][0]["pos_id"]
    );
    assert_ne!(
        saved["word"]["meanings"]["pos"][0]["senses"][0]["id"],
        old_sense_id
    );
    assert_eq!(
        saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"],
        json!([])
    );
}

#[sqlx::test]
async fn forms_surface_warning_allows_reverse_workspaces_after_acknowledgement(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    seed_dictionary_word(&pool, "workspace").await;
    let (status, workspace_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "workspace"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "workspace 检测失败：{workspace_detection}"
    );
    let (status, workspace) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": workspace_detection["detection_id"],
            "headwords": workspace_detection["builtin_dictionary"]["headwords"],
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "workspace 创建失败：{workspace}"
    );
    let workspace_id = workspace["word"]["id"].as_str().unwrap().to_owned();

    // 先建立另一个明确保存了 headword/base form=workspaces 的 entry，再反向保存
    // workspace.plural=workspaces；没有任何英语复数推导参与这个 fixture。
    let existing = create_ready_draft(&state, &pool, &bearer, "workspaces").await;
    let existing_id = existing["word"]["id"].as_str().unwrap().to_owned();
    let concurrent = create_ready_draft(&state, &pool, &bearer, "concurrent-owner").await;
    let concurrent_id = concurrent["word"]["id"].as_str().unwrap().to_owned();

    let mut forms = workspace["word"]["forms"].clone();
    forms["pos"][0]["base_form"]["variants"][0]["pronunciations"][0]["dict_phonetic"] =
        json!("/workspace/");
    forms["pos"][0]["base_form"]["variants"][0]["pronunciations"][0]["actual_pron"] =
        json!("workspace");
    let plural_variant_id = Uuid::now_v7();
    forms["pos"][0]["form_groups"][0]["slots"] = json!([{
        "id": Uuid::now_v7(),
        "form_type": "plural",
        "variants": [{
            "id": plural_variant_id,
            "dialect": "common",
            "spelling": "workspaces",
            "origin": "manual",
            "pronunciations": [{
                "id": Uuid::now_v7(),
                "dict_phonetic": "/workspaces/",
                "actual_pron": "workspaces",
                "style": "normal"
            }]
        }]
    }]);

    let impact_uri = format!("{ROOT}/entries/{workspace_id}/steps/forms/impact");
    let (status, preview) = call(
        &state,
        Method::POST,
        &impact_uri,
        &bearer,
        None,
        Some(json!({"base_revision": 1, "content": forms.clone()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "反向 Forms 预检失败：{preview}");
    assert_eq!(preview["requires_confirmation"], false);
    assert!(preview["confirmation_token"].is_null());
    let preview_items = preview["surface_match_page"]["items"]
        .as_array()
        .expect("必须返回 surface warning 首页");
    assert!(preview_items.iter().any(|item| {
        item["match_category"] == "form_headword"
            && item["candidate"]["candidate_node_id"] == plural_variant_id.to_string()
            && item["existing"]["word_id"] == existing_id
    }));
    assert!(preview_items.iter().any(|item| {
        item["match_category"] == "form_form"
            && item["candidate"]["candidate_node_id"] == plural_variant_id.to_string()
            && item["existing"]["word_id"] == existing_id
    }));
    assert!(
        preview_items
            .iter()
            .all(|item| { item["existing"]["word_id"] != workspace_id }),
        "同一 entry 的 headword/slot 同形必须被整体排除"
    );

    let save_uri = format!("{ROOT}/entries/{workspace_id}/steps/forms");
    let (status, required) = call(
        &state,
        Method::PUT,
        &save_uri,
        &bearer,
        None,
        Some(json!({
            "base_revision": 1,
            "intent": "complete",
            "content": forms.clone(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "缺少确认必须拒绝：{required}");
    assert_eq!(required["code"], "surface_match_acknowledgement_required");
    let revision_after_cancel: i64 =
        sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(&workspace_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(revision_after_cancel, 1, "取消/缺 token 不得推进 revision");

    let surface_token = required["meta"]["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .expect("单页 warning 必须提供终页 token");

    // 模拟另一个 surface writer 在 preview 后提交了新的跨 entry form source；
    // Forms save 必须在统一锁内重查完整集合，而不能消费旧 snapshot 静默放行。
    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET surface = 'workspaces', normalized_surface = 'workspaces',
            source_revision = source_revision + 1,
            event_offset = nextval('lexicon.surface_projection_event_offset_seq')
        WHERE entry_id = $1 AND source_kind = 'form' AND is_deleted = FALSE
        "#,
    )
    .bind(Uuid::parse_str(&concurrent_id).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let (status, changed) = call(
        &state,
        Method::PUT,
        &save_uri,
        &bearer,
        None,
        Some(json!({
            "base_revision": 1,
            "intent": "complete",
            "confirmed_surface_match_token": surface_token,
            "content": forms.clone(),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "锁后新增 match 必须重确认：{changed}"
    );
    assert_eq!(changed["code"], "surface_matches_changed");
    assert!(
        changed["meta"]["surface_match_page"]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| {
                item["match_category"] == "form_form"
                    && item["existing"]["word_id"] == concurrent_id
            })),
        "409 新首页必须包含锁后新增的 form source"
    );
    let revision_after_change: i64 =
        sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(&workspace_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(revision_after_change, 1, "重确认前不得推进 revision");
    let replacement_surface_token =
        changed["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .expect("409 新终页必须提供 replacement token");

    let (status, saved) = call(
        &state,
        Method::PUT,
        &save_uri,
        &bearer,
        None,
        Some(json!({
            "base_revision": 1,
            "intent": "complete",
            "confirmed_surface_match_token": replacement_surface_token,
            "content": forms,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "明确确认后应允许保存：{saved}");
    assert_eq!(saved["word"]["revision"], 2);
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["form_groups"][0]["slots"][0]["variants"][0]["spelling"],
        "workspaces"
    );

    let evidence: (i64, String, Vec<String>) = sqlx::query_as(
        r#"
        SELECT forms_revision, forms_content_digest, match_ids
        FROM lexicon.entry_forms_surface_acknowledgements
        WHERE entry_id = $1
        "#,
    )
    .bind(Uuid::parse_str(&workspace_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(evidence.0, 2);
    assert!(!evidence.1.is_empty());
    assert!(
        evidence.2.len() >= 2,
        "headword 与 form source 证据都应保留"
    );

    let saved_forms = saved["word"]["forms"].clone();
    let (status, reused) = call(
        &state,
        Method::POST,
        &impact_uri,
        &bearer,
        None,
        Some(json!({"base_revision": 2, "content": saved_forms})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "证据复用预检失败：{reused}");
    assert!(reused.get("surface_match_page").is_none());

    let (status, saved_again) = call(
        &state,
        Method::PUT,
        &save_uri,
        &bearer,
        None,
        Some(json!({
            "base_revision": 2,
            "intent": "save",
            "content": saved["word"]["forms"],
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "相同 canonical forms 证据应可复用：{saved_again}"
    );
    assert_eq!(saved_again["word"]["revision"], 3);
}

#[sqlx::test]
async fn forms_surface_and_downstream_impact_require_both_terminal_tokens(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis.clone());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let source = create_ready_draft(&state, &pool, &bearer, "combined-source").await;
    let existing = create_ready_draft(&state, &pool, &bearer, "combined-collision").await;
    let entry_id = source["word"]["id"].as_str().unwrap();
    let base_revision = source["word"]["revision"].as_i64().unwrap();
    let mut forms = source["word"]["forms"].clone();
    forms["pos"][0]["pos"] = json!("adjective");
    forms["pos"][0]["form_groups"][0]["slots"][0]["form_type"] = json!("comparative");
    forms["pos"][0]["form_groups"][0]["slots"][0]["id"] = json!(Uuid::now_v7());
    let candidate_node_id = Uuid::now_v7();
    forms["pos"][0]["form_groups"][0]["slots"][0]["variants"][0]["id"] = json!(candidate_node_id);
    forms["pos"][0]["form_groups"][0]["slots"][0]["variants"][0]["spelling"] =
        json!("combined-collision");
    forms["pos"][0]["form_groups"][0]["slots"][0]["variants"][0]["pronunciations"][0]["id"] =
        json!(Uuid::now_v7());

    let impact_uri = format!("{ROOT}/entries/{entry_id}/steps/forms/impact");
    let (status, preview) = call(
        &state,
        Method::POST,
        &impact_uri,
        &bearer,
        None,
        Some(json!({
            "base_revision": base_revision,
            "content": forms.clone(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "双确认预检失败：{preview}");
    assert_eq!(preview["requires_confirmation"], true);
    assert!(!preview["affected"].as_array().unwrap().is_empty());
    assert!(
        preview.get("confirmation_token").is_none() || preview["confirmation_token"].is_null(),
        "surface 存在时不得提前签发旧 impact token"
    );
    let page = &preview["surface_match_page"];
    assert!(
        page["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| {
                item["match_category"] == "form_headword"
                    && item["candidate"]["candidate_node_id"] == candidate_node_id.to_string()
                    && item["existing"]["word_id"] == existing["word"]["id"]
            }))
    );
    let surface_token = page["surface_confirmation_token"]
        .as_str()
        .expect("surface 终页 token 缺失");
    let impact_token = page["impact_confirmation_token"]
        .as_str()
        .expect("impact 终页 token 缺失");
    Uuid::parse_str(impact_token).expect("impact token 必须为 UUID wire");

    let save_uri = format!("{ROOT}/entries/{entry_id}/steps/forms");
    let (status, missing_impact) = call(
        &state,
        Method::PUT,
        &save_uri,
        &bearer,
        None,
        Some(json!({
            "base_revision": base_revision,
            "intent": "save",
            "confirmed_surface_match_token": surface_token,
            "content": forms.clone(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "只交 surface token 必须失败");
    assert_eq!(missing_impact["code"], "downstream_confirmation_required");
    let unchanged_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(entry_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(unchanged_revision, base_revision);

    let snapshot_id = page["snapshot_id"].as_str().expect("snapshot id 缺失");
    let mut redis_connection = redis.get().await.expect("应能取得测试 Redis 连接");
    deadpool_redis::redis::cmd("DEL")
        .arg(format!("lexicon:surface-snapshot:{snapshot_id}"))
        .query_async::<i64>(&mut redis_connection)
        .await
        .expect("应能使 Forms snapshot 过期");
    let (status, expired) = call(
        &state,
        Method::PUT,
        &save_uri,
        &bearer,
        None,
        Some(json!({
            "base_revision": base_revision,
            "intent": "save",
            "confirmed_surface_match_token": surface_token,
            "confirmed_impact_token": impact_token,
            "content": forms.clone(),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GONE,
        "过期双 token 必须返回 410：{expired}"
    );
    assert_eq!(expired["code"], "surface_match_snapshot_expired");
    let revision_after_expiry: i64 =
        sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(entry_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(revision_after_expiry, base_revision);

    let (status, refreshed) = call(
        &state,
        Method::POST,
        &impact_uri,
        &bearer,
        None,
        Some(json!({
            "base_revision": base_revision,
            "content": forms.clone(),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "过期后重新 impact 失败：{refreshed}"
    );
    let refreshed_snapshot_id = refreshed["surface_match_page"]["snapshot_id"]
        .as_str()
        .expect("重新 impact 后缺 snapshot id")
        .to_owned();
    let refreshed_surface_token = refreshed["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .expect("重新 impact 后缺 surface token")
        .to_owned();
    let refreshed_impact_token = refreshed["surface_match_page"]["impact_confirmation_token"]
        .as_str()
        .expect("重新 impact 后缺 impact token")
        .to_owned();

    // preview 后若唯一外部 match 被另一个合法 writer 删除，ordinary warning 已消失；
    // dual impact token 仍须按完整 owner/content/affected binding 验证并允许保存。
    let existing_id = existing["word"]["id"].as_str().unwrap();
    let (status, deleted) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{existing_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": existing["word"]["revision"],
            "base_lifecycle_revision": existing["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "并发删除外部 match 失败：{deleted}"
    );

    let (status, saved) = call(
        &state,
        Method::PUT,
        &save_uri,
        &bearer,
        None,
        Some(json!({
            "base_revision": base_revision,
            "intent": "save",
            "confirmed_surface_match_token": refreshed_surface_token,
            "confirmed_impact_token": refreshed_impact_token,
            "content": forms,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "双 token 一次保存应成功：{saved}");
    assert_eq!(saved["word"]["revision"], base_revision + 1);
    assert_eq!(saved["word"]["forms"]["pos"][0]["pos"], "adjective");
    let refreshed_snapshot_exists: i64 = deadpool_redis::redis::cmd("EXISTS")
        .arg(format!("lexicon:surface-snapshot:{refreshed_snapshot_id}"))
        .query_async(&mut redis_connection)
        .await
        .expect("应能检查成功消费后的 snapshot");
    assert_eq!(
        refreshed_snapshot_exists, 0,
        "即使 warning 锁后全部消失，成功保存后也应清理双 token snapshot"
    );
}

#[sqlx::test]
async fn forms_impact_ignores_reconciled_default_meaning_placeholders(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    seed_dictionary_word(&pool, "tomato").await;

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "tomato"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "检测失败：{detection}");
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": detection["builtin_dictionary"]["headwords"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建失败：{created}");
    let entry_id = created["word"]["id"].as_str().unwrap();
    let revision = created["word"]["revision"].as_i64().unwrap();

    let mut content_only = created["word"]["forms"].clone();
    content_only["pos"][0]["base_form"]["variants"][0]["pronunciations"][0]["dict_phonetic"] =
        json!("/təˈmɑːtoʊ/");
    content_only["pos"][0]["base_form"]["variants"][0]["pronunciations"][0]["actual_pron"] =
        json!("təˈmɑːtoʊ");
    let (status, content_impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({"base_revision": revision, "content": content_only})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "内容补全 impact 失败：{content_impact}"
    );
    assert_eq!(content_impact["requires_confirmation"], false);

    let mut replaced = created["word"]["forms"].clone();
    replaced["pos"][0]["pos"] = json!("adjective");
    replaced["pos"][0]["form_groups"] = json!([{
        "id": Uuid::now_v7(),
        "is_regular": true,
        "slots": [{
            "id": Uuid::now_v7(),
            "form_type": "comparative",
            "variants": [{
                "id": Uuid::now_v7(),
                "dialect": "common",
                "spelling": "more tomato",
                "origin": "manual",
                "pronunciations": [{
                    "id": Uuid::now_v7(),
                    "dict_phonetic": "/mɔːr təˈmɑːtoʊ/",
                    "actual_pron": "mɔːr təˈmɑːtoʊ",
                    "style": "normal"
                }]
            }]
        }]
    }]);
    let (status, placeholder_impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({"base_revision": revision, "content": replaced})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "默认占位 reconcile impact 失败：{placeholder_impact}"
    );
    assert_eq!(placeholder_impact["requires_confirmation"], false);
    assert_eq!(placeholder_impact["affected"], json!([]));

    let mut completed_forms = content_only;
    completed_forms["pos"][0]["form_groups"] = json!([{
        "id": Uuid::now_v7(),
        "is_regular": true,
        "slots": [{
            "id": Uuid::now_v7(),
            "form_type": "plural",
            "variants": [{
                "id": Uuid::now_v7(),
                "dialect": "common",
                "spelling": "tomatoes",
                "origin": "manual",
                "pronunciations": [{
                    "id": Uuid::now_v7(),
                    "dict_phonetic": "/təˈmɑːtoʊz/",
                    "actual_pron": "təˈmɑːtoʊz",
                    "style": "normal"
                }]
            }]
        }]
    }]);
    let (status, completed) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "complete",
            "content": completed_forms,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "完成 forms 失败：{completed}");

    let mut named_group_meanings = completed["word"]["meanings"].clone();
    named_group_meanings["sense_groups"][0]["name_zh"] = json!("共享但不会被删除的分组");
    let (status, with_named_group) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": completed["word"]["revision"],
            "intent": "save",
            "content": named_group_meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "命名分组保存失败：{with_named_group}"
    );
    let (status, named_group_impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({
            "base_revision": with_named_group["word"]["revision"],
            "content": replaced,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "保留命名分组的 impact 失败：{named_group_impact}"
    );
    assert_eq!(named_group_impact["requires_confirmation"], false);
    assert_eq!(named_group_impact["affected"], json!([]));
}

#[sqlx::test]
async fn stable_node_slots_and_parent_bindings_are_enforced(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let ready = create_ready_draft(&state, &pool, &bearer, "stable-bindings").await;
    let entry_id = ready["word"]["id"].as_str().unwrap();
    let revision = ready["word"]["revision"].as_i64().unwrap();

    let mut replaced_base_slot = ready["word"]["forms"].clone();
    replaced_base_slot["pos"][0]["base_form"]["id"] = json!(Uuid::now_v7());
    let (status, rejected) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "content": replaced_base_slot,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "固定 forms 槽位换 ID 应拒绝：{rejected}"
    );
    assert!(has_issue(&rejected, "stable_node_id_changed"));

    let mut rebound_form_variant = ready["word"]["forms"].clone();
    let base_variant_id = rebound_form_variant["pos"][0]["base_form"]["variants"][0]["id"].clone();
    let derived_variant_id =
        rebound_form_variant["pos"][0]["form_groups"][0]["slots"][0]["variants"][0]["id"].clone();
    rebound_form_variant["pos"][0]["base_form"]["variants"][0]["id"] = derived_variant_id;
    rebound_form_variant["pos"][0]["form_groups"][0]["slots"][0]["variants"][0]["id"] =
        base_variant_id;
    let (status, rejected) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "content": rebound_form_variant,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "同类型 forms ID 换父节点应拒绝：{rejected}"
    );
    assert!(has_issue(&rejected, "node_binding_changed"));

    let original_meanings = ready["word"]["meanings"].clone();
    let mut replaced_definition_content = original_meanings.clone();
    replaced_definition_content["pos"][0]["senses"][0]["definitions"][0]["content_id"] =
        json!(Uuid::now_v7());
    let mut replaced_sentence_english = original_meanings.clone();
    replaced_sentence_english["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["id"] =
        json!(Uuid::now_v7());
    let mut replaced_sentence_chinese = original_meanings.clone();
    replaced_sentence_chinese["pos"][0]["senses"][0]["sentences"][0]["zh_text_id"] =
        json!(Uuid::now_v7());
    for (label, content) in [
        ("中文 content_id", replaced_definition_content),
        ("TextVariantV2.id", replaced_sentence_english),
        ("sentence zh_text_id", replaced_sentence_chinese),
    ] {
        let (status, rejected) = call(
            &state,
            Method::PUT,
            &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
            &bearer,
            None,
            Some(json!({
                "base_revision": revision,
                "intent": "save",
                "content": content,
            })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "已有 {label} 换 ID 应拒绝：{rejected}"
        );
        assert!(has_issue(&rejected, "stable_node_id_changed"));
    }

    let mut rebound_text_nodes = original_meanings.clone();
    let definition_content_id =
        rebound_text_nodes["pos"][0]["senses"][0]["definitions"][0]["content_id"].clone();
    let sentence_zh_id =
        rebound_text_nodes["pos"][0]["senses"][0]["sentences"][0]["zh_text_id"].clone();
    rebound_text_nodes["pos"][0]["senses"][0]["definitions"][0]["content_id"] = sentence_zh_id;
    rebound_text_nodes["pos"][0]["senses"][0]["sentences"][0]["zh_text_id"] = definition_content_id;
    let (status, rejected) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "content": rebound_text_nodes,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "同类型文本 ID 换父节点应拒绝：{rejected}"
    );
    assert!(has_issue(&rejected, "node_binding_changed"));

    let legacy_text_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.nodes (
            id, entry_id, node_type, node_role, removed_from_draft_at
        ) VALUES ($1, $2, 'text_variant', 'legacy', now())
        "#,
    )
    .bind(legacy_text_id)
    .bind(Uuid::parse_str(entry_id).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let mut reused_legacy_text = original_meanings.clone();
    reused_legacy_text["pos"][0]["senses"][0]["sentences"][0]["zh_text_id"] = json!(legacy_text_id);
    let (status, rejected) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "content": reused_legacy_text,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "无法回填绑定的历史 ID 不能静默重新认领：{rejected}"
    );
    assert!(has_issue(&rejected, "node_binding_unknown"));

    let definition_id = Uuid::now_v7();
    let mut with_missing_slots = original_meanings;
    with_missing_slots["pos"][0]["senses"][0]["definitions"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "definition_mode": "en_definition",
            "id": definition_id,
            "level": "A1",
            "content": {
                "mode": "distinguish",
                "source_dialect": "uk",
                "uk": {"state": "missing"},
                "us": {"state": "missing"}
            }
        }));
    let (status, with_missing_slots) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "content": with_missing_slots,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "missing 槽位草稿应可保存：{with_missing_slots}"
    );

    let mut create_ready_slot = with_missing_slots["word"]["meanings"].clone();
    let added_definition = create_ready_slot["pos"][0]["senses"][0]["definitions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|definition| definition["id"] == json!(definition_id))
        .expect("应找到新增的英文释义");
    let new_text_id = Uuid::now_v7();
    added_definition["content"]["uk"] = json!({
        "state": "ready",
        "variant": {
            "id": new_text_id,
            "value": rich_text("newly created from missing"),
            "origin": "manual"
        }
    });
    let (status, created_slot) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": with_missing_slots["word"]["revision"],
            "intent": "save",
            "content": create_ready_slot,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "missing -> ready 应允许新 UUID：{created_slot}"
    );
    assert!(created_slot.to_string().contains(&new_text_id.to_string()));

    let stored_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(entry_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        stored_revision,
        revision + 2,
        "只有两次合法保存可推进 revision"
    );
}

#[sqlx::test]
async fn forms_and_meanings_reject_aggregate_node_limit_before_writes(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let ready = create_ready_draft(&state, &pool, &bearer, "node-limit").await;
    let entry_id = ready["word"]["id"].as_str().unwrap();
    let entry_uuid = Uuid::parse_str(entry_id).unwrap();
    let revision = ready["word"]["revision"].as_i64().unwrap();
    let node_count_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.nodes WHERE entry_id = $1")
            .bind(entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();

    let oversized_groups = (0..2_001)
        .map(|_| {
            json!({
                "id": Uuid::now_v7(),
                "is_regular": false,
                "slots": []
            })
        })
        .collect::<Vec<_>>();
    let mut oversized_forms = ready["word"]["forms"].clone();
    oversized_forms["pos"][0]["form_groups"] = json!(oversized_groups);
    let (status, rejected_forms) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "content": oversized_forms,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "forms 超限应返回结构化 422：{rejected_forms}"
    );
    assert_eq!(rejected_forms["code"], "validation_failed");
    assert!(has_issue(&rejected_forms, "aggregate_node_limit_exceeded"));

    let mut oversized_meanings = ready["word"]["meanings"].clone();
    oversized_meanings["sense_groups"]
        .as_array_mut()
        .unwrap()
        .extend((0..2_001).map(|index| {
            json!({
                "id": Uuid::now_v7(),
                "name_zh": format!("超限{index}"),
                "name_en": format!("Overflow {index}")
            })
        }));
    let (status, rejected_meanings) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": revision,
            "intent": "save",
            "content": oversized_meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "meanings 超限应返回结构化 422：{rejected_meanings}"
    );
    assert_eq!(rejected_meanings["code"], "validation_failed");
    assert!(has_issue(
        &rejected_meanings,
        "aggregate_node_limit_exceeded"
    ));

    let (stored_revision, node_count_after): (i64, i64) = sqlx::query_as(
        r#"
        SELECT entry.revision, count(node.id)::bigint
        FROM lexicon.entries entry
        LEFT JOIN lexicon.nodes node ON node.entry_id = entry.id
        WHERE entry.id = $1
        GROUP BY entry.revision
        "#,
    )
    .bind(entry_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored_revision, revision);
    assert_eq!(node_count_after, node_count_before);
}
