//! 智能词库从检测、建稿、分步保存到不可变发布的主链路契约测试。

use std::{collections::HashSet, time::Duration};

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
    config::SmartLexiconV3Flags,
    lexicon::detection_store::DetectionStore,
    lexicon::dto::{DetectLexiconSurfaceResponseV3, DetectWordResponseV2, SurfacePolicyNameV2},
    lexicon::normalization::{HEADWORD_NORMALIZATION_VERSION, normalize_headword, sha256_json},
    lexicon::repository::LexiconRepository,
    lexicon::surface_backfill::{
        SURFACE_WRITER_VERSION, execute_surface_cutover, run_surface_backfill,
        run_surface_cutover_preflight, run_surface_parity, surface_cutover_artifact_sha256,
    },
    lexicon::surface_snapshot,
    lexicon::v3_migration::{apply, approve, dry_run, enable_publication_canary, verify},
    lexicon::validation::MAX_STEP_CONTENT_BODY_BYTES,
    platform,
    state::AppState,
};

const ROOT: &str = "/api/v1/admin/lexicon";
const CONCURRENCY_TIMEOUT: Duration = Duration::from_secs(30);

async fn await_database_lock_waiters(pool: &PgPool, expected: i64) {
    let deadline = tokio::time::Instant::now() + CONCURRENCY_TIMEOUT;
    loop {
        let waiting: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM pg_stat_activity
            WHERE datname = current_database()
              AND state = 'active'
              AND wait_event_type = 'Lock'
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting >= expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "等待 {expected} 个数据库锁排队者超时，当前 {waiting}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn test_redis_url() -> String {
    std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_owned())
}

async fn seed_admin(pool: &PgPool) -> Uuid {
    seed_admin_with_role(pool, AdminRole::Admin).await
}

async fn seed_admin_with_role(pool: &PgPool, role: AdminRole) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("lexicon-{}", id.simple()),
            display_name: "词库测试管理员".to_owned(),
            password_hash: "hashed-password".to_owned(),
            role,
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

async fn call_problem(
    state: &AppState,
    method: Method,
    uri: &str,
    bearer: &str,
    idempotency_key: Option<Uuid>,
    body: Value,
) -> (StatusCode, String, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(idempotency_key) = idempotency_key {
        builder = builder.header("Idempotency-Key", idempotency_key.to_string());
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(
            builder
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "Problem Details 响应应为 JSON：{error}，body={}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, content_type, body)
}

fn rich_text(text: &str) -> Value {
    json!({"version": 1, "text": text, "spans": [], "liaisons": []})
}

fn has_issue(body: &Value, expected_code: &str) -> bool {
    body["field_issues"]
        .as_array()
        .is_some_and(|issues| issues.iter().any(|issue| issue["code"] == expected_code))
}

fn json_uuids(value: &Value) -> HashSet<Uuid> {
    fn collect(value: &Value, output: &mut HashSet<Uuid>) {
        match value {
            Value::String(value) => {
                if let Ok(id) = Uuid::parse_str(value) {
                    output.insert(id);
                }
            }
            Value::Array(values) => {
                for value in values {
                    collect(value, output);
                }
            }
            Value::Object(values) => {
                for value in values.values() {
                    collect(value, output);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    let mut output = HashSet::new();
    collect(value, &mut output);
    output
}

fn complete_v3_forms_fixture() -> Value {
    let noun_pos_id = Uuid::now_v7();
    let shared_form_id = Uuid::now_v7();
    let alternate_base_id = Uuid::now_v7();
    let first_group_id = Uuid::now_v7();
    let second_group_id = Uuid::now_v7();
    json!({
        "pos": [{
            "pos_id": noun_pos_id,
            "pos": "noun",
            "dialect_rules": {
                "spelling_mode": "distinguish",
                "phonetic_mode": "distinguish"
            },
            "forms": [{
                "id": shared_form_id,
                "form_type": "base",
                "regional_variants": {
                    "mode": "uk_us",
                    "uk": {
                        "id": Uuid::now_v7(),
                        "dialect": "uk",
                        "spelling": "harbour",
                        "origin": "manual",
                        "pronunciations": [{
                            "id": Uuid::now_v7(),
                            "dict_phonetic": "/ˈhɑːbə/",
                            "actual_pron": "hɑːbə",
                            "style": "normal"
                        }]
                    },
                    "us": {
                        "id": Uuid::now_v7(),
                        "dialect": "us",
                        "spelling": "harbor",
                        "origin": "manual",
                        "pronunciations": [{
                            "id": Uuid::now_v7(),
                            "dict_phonetic": "/ˈhɑrbər/",
                            "actual_pron": "hɑrbər",
                            "style": "normal"
                        }]
                    }
                }
            }, {
                "id": alternate_base_id,
                "form_type": "base",
                "regional_variants": {
                    "mode": "uk_us",
                    "uk": {
                        "id": Uuid::now_v7(),
                        "dialect": "uk",
                        "spelling": "harbour",
                        "origin": "manual",
                        "pronunciations": [{
                            "id": Uuid::now_v7(),
                            "dict_phonetic": "/ˈhɑːbə/",
                            "actual_pron": "hɑːbə",
                            "style": "normal"
                        }]
                    },
                    "us": {
                        "id": Uuid::now_v7(),
                        "dialect": "us",
                        "spelling": "harbor",
                        "origin": "manual",
                        "pronunciations": [{
                            "id": Uuid::now_v7(),
                            "dict_phonetic": "/ˈhɑrbər/",
                            "actual_pron": "hɑrbər",
                            "style": "normal"
                        }]
                    }
                }
            }],
            "form_groups": [{
                "id": first_group_id,
                "is_regular": true,
                "members": [
                    {"id": Uuid::now_v7(), "form_id": shared_form_id},
                    {"id": Uuid::now_v7(), "form_id": alternate_base_id}
                ]
            }, {
                "id": second_group_id,
                "is_regular": false,
                "members": [{"id": Uuid::now_v7(), "form_id": shared_form_id}]
            }]
        }]
    })
}

fn complete_v3_meanings_fixture(pos_id: Value) -> Value {
    let sense_group_id = Uuid::now_v7();
    let grammar_id = Uuid::now_v7();
    json!({
        "sense_groups": [{
            "id": sense_group_id,
            "name_zh": "核心义",
            "name_en": "core"
        }],
        "pos": [{
            "pos_id": pos_id,
            "grammar_structures": [{
                "id": grammar_id,
                "variants": [{
                    "id": Uuid::now_v7(),
                    "dialect": "common",
                    "content": rich_text("countable noun")
                }]
            }],
            "senses": [{
                "id": Uuid::now_v7(),
                "sub_pos": "N-COUNT",
                "level": "A1",
                "sense_group_id": sense_group_id,
                "frequency": "100",
                "depends_on_context": false,
                "definitions": [{
                    "definition_mode": "zh_definition",
                    "id": Uuid::now_v7(),
                    "content_id": Uuid::now_v7(),
                    "level": "A1",
                    "grammar_structure_id": grammar_id,
                    "content": rich_text("港口")
                }],
                "sentences": [],
                "relations": []
            }]
        }]
    })
}

async fn create_v3_with_complete_forms(state: &AppState, pool: &PgPool, bearer: &str) -> Value {
    let dictionary_term_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM dictionary.active_terms WHERE normalized_term = 'harbour')",
    )
    .fetch_one(pool)
    .await
    .expect("应能检查 V3 fixture 词典词条");
    if !dictionary_term_exists {
        seed_dictionary_word(pool, "harbour").await;
    }
    let (status, detection) = call(
        state,
        Method::POST,
        &format!("{ROOT}/detections"),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "harbour"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let mut create_input = json!({
        "schema_version": 3,
        "detection_id": detection["detection_id"],
        "kind": "word"
    });
    if let Some(token) = detection["surface_match_page"]["surface_confirmation_token"].as_str() {
        create_input["confirmed_surface_match_token"] = json!(token);
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
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let entry_id = created["word"]["id"].as_str().unwrap();
    let forms_content = complete_v3_forms_fixture();
    let (status, impact) = call(
        state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "content": forms_content.clone()
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    let mut forms_input = json!({
        "schema_version": 3,
        "base_revision": 1,
        "intent": "complete",
        "content": forms_content
    });
    if let Some(token) = impact["confirmation_token"].as_str() {
        forms_input["confirmed_impact_token"] = json!(token);
    }
    if let Some(token) = impact["surface_match_page"]["impact_confirmation_token"].as_str() {
        forms_input["confirmed_impact_token"] = json!(token);
    }
    if let Some(token) = impact["surface_match_page"]["surface_confirmation_token"].as_str() {
        forms_input["confirmed_surface_match_token"] = json!(token);
    }
    let (mut status, mut saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        bearer,
        None,
        Some(forms_input.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT && saved["code"] == "surface_match_acknowledgement_required" {
        let mut confirmed = forms_input;
        confirmed["confirmed_surface_match_token"] =
            saved["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
        (status, saved) = call(
            state,
            Method::PUT,
            &format!("{ROOT}/entries/{entry_id}/steps/forms"),
            bearer,
            None,
            Some(confirmed),
        )
        .await;
    }
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved
}

async fn save_v3_forms_after_impact(
    state: &AppState,
    bearer: &str,
    entry_id: &str,
    base_revision: i64,
    intent: &str,
    content: Value,
) -> (Value, Value) {
    let (status, impact) = call(
        state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": base_revision,
            "content": content.clone()
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    let mut input = json!({
        "schema_version": 3,
        "base_revision": base_revision,
        "intent": intent,
        "content": content
    });
    if let Some(token) = impact["confirmation_token"].as_str() {
        input["confirmed_impact_token"] = json!(token);
    }
    if let Some(page) = impact["surface_match_page"].as_object() {
        if let Some(token) = page
            .get("surface_confirmation_token")
            .and_then(Value::as_str)
        {
            input["confirmed_surface_match_token"] = json!(token);
        }
        if let Some(token) = page
            .get("impact_confirmation_token")
            .and_then(Value::as_str)
        {
            input["confirmed_impact_token"] = json!(token);
        }
    }
    let (status, saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        bearer,
        None,
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    (impact, saved)
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
    create_ready_draft_with_headwords(state, pool, bearer, headword, None).await
}

async fn create_legacy_v3_empty_skeleton(state: &AppState, bearer: &str, surface: &str) -> Uuid {
    let (status, detection) = call(
        state,
        Method::POST,
        &format!("{ROOT}/detections"),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let (status, created) = call(
        state,
        Method::POST,
        &format!("{ROOT}/entries"),
        bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap()
}

/// 与 `create_ready_draft` 相同的建稿链路，但可以覆盖词头以构造 distinguish 词条。
async fn create_ready_draft_with_headwords(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
    headword: &str,
    headwords: Option<Value>,
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
        "headwords": headwords
            .unwrap_or_else(|| detection["builtin_dictionary"]["headwords"].clone()),
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
    // distinguish 词条的骨架自带 uk/us 两行，unified 只有 common 一行；统一按骨架逐行补齐。
    let base_variants = forms["pos"][0]["base_form"]["variants"]
        .as_array()
        .expect("基本形应带方言行")
        .clone();
    for index in 0..base_variants.len() {
        let pronunciation =
            &mut forms["pos"][0]["base_form"]["variants"][index]["pronunciations"][0];
        pronunciation["dict_phonetic"] = json!("/test/");
        pronunciation["actual_pron"] = json!("test");
    }
    forms["pos"][0]["form_groups"][0]["slots"] = json!([{
        "id": Uuid::now_v7(),
        "form_type": "plural",
        "variants": base_variants
            .iter()
            .map(|variant| json!({
                "id": Uuid::now_v7(),
                "dialect": variant["dialect"],
                "spelling": format!("{}s", variant["spelling"].as_str().unwrap()),
                "origin": "manual",
                "pronunciations": [{
                    "id": Uuid::now_v7(),
                    "dict_phonetic": "/tests/",
                    "actual_pron": "tests",
                    "style": "normal"
                }]
            }))
            .collect::<Vec<_>>()
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
    let grammar_variants = meanings["pos"][0]["grammar_structures"][0]["variants"]
        .as_array()
        .expect("语法结构应带方言行")
        .len();
    for index in 0..grammar_variants {
        meanings["pos"][0]["grammar_structures"][0]["variants"][index]["content"] =
            rich_text("used as a noun");
    }
    meanings["pos"][0]["senses"][0]["sub_pos"] = json!("N-COUNT");
    meanings["pos"][0]["senses"][0]["frequency"] = json!("50");
    meanings["pos"][0]["senses"][0]["definitions"][0]["grammar_structure_id"] =
        meanings["pos"][0]["grammar_structures"][0]["id"].clone();
    meanings["pos"][0]["senses"][0]["definitions"][0]["content"] =
        rich_text(&format!("{headword} 的释义"));
    let example = rich_text(&format!("A {headword} example."));
    let en_text = &mut meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"];
    if en_text["mode"] == "distinguish" {
        // 骨架只把基准侧填成 ready，另一侧是 missing，两侧都要有内容才算完成。
        for side in ["uk", "us"] {
            if en_text[side]["state"] == "ready" {
                en_text[side]["variant"]["value"] = example.clone();
            } else {
                en_text[side] = json!({
                    "state": "ready",
                    "variant": {
                        "id": Uuid::now_v7(),
                        "value": example.clone(),
                        "origin": "manual"
                    }
                });
            }
        }
    } else {
        en_text["common"]["value"] = example;
    }
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

/// 把词条唯一那条例句的英文正文换成 `text`，保存后返回新的 envelope。
///
/// 请求体直接由读到的 meanings 改出来，因此顺带覆盖「客户端把只读的 associations
/// 原样回传，服务端必须丢弃」这条。
async fn save_example_sentence(state: &AppState, bearer: &str, word: &Value, text: &str) -> Value {
    let entry_id = word["word"]["id"].as_str().unwrap().to_owned();
    let mut meanings = word["word"]["meanings"].clone();
    let en_text = &mut meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"];
    if en_text["mode"] == "distinguish" {
        for side in ["uk", "us"] {
            en_text[side]["variant"]["value"] = rich_text(text);
        }
    } else {
        en_text["common"]["value"] = rich_text(text);
    }
    let (status, saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        bearer,
        None,
        Some(json!({
            "base_revision": word["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "保存例句正文失败：{saved}");
    saved
}

/// 建稿 → 发布，返回发布后的 envelope。自动关联的目标必须是已发布词条。
async fn create_and_publish(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
    headword: &str,
) -> Value {
    let draft = create_ready_draft(state, pool, bearer, headword).await;
    let (status, published) = publish_ready(state, bearer, &draft).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "发布 {headword} 失败：{published}"
    );
    published
}

fn first_sentence(word: &Value) -> &Value {
    &word["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]
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

/// 当前发布内容的 surface 投影绑在哪条 publication 上——切版本时必须跟着走。
async fn live_surface_publication_ids(pool: &PgPool, entry_id: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar(
        r#"
        SELECT DISTINCT publication_id
        FROM lexicon.surface_sources
        WHERE entry_id = $1
          AND content_scope = 'current_publication'
          AND NOT is_deleted
        "#,
    )
    .bind(entry_id)
    .fetch_all(pool)
    .await
    .expect("应能读取 surface 投影绑定的 publication")
}

async fn activation_write_fingerprint(pool: &PgPool, entry_id: Uuid) -> Value {
    sqlx::query_scalar(
        r#"
        SELECT jsonb_build_object(
            'entry', (
                SELECT to_jsonb(entry_row)
                FROM lexicon.entries entry_row
                WHERE entry_row.id = $1
            ),
            'surfaces', COALESCE((
                SELECT jsonb_agg(
                    to_jsonb(surface_row)
                    ORDER BY surface_row.content_scope,
                             surface_row.source_id,
                             surface_row.dialect_scope,
                             surface_row.publication_id
                )
                FROM lexicon.surface_sources surface_row
                WHERE surface_row.entry_id = $1
            ), '[]'::jsonb),
            'outbox_count', (
                SELECT count(*)
                FROM platform.outbox_events event
                WHERE event.aggregate_id = $1
            ),
            'audit_count', (
                SELECT count(*)
                FROM audit.admin_actions action
                WHERE action.resource_id = $1
            ),
            'idempotency_count', (
                SELECT count(*)
                FROM platform.idempotency_records record
                WHERE record.resource_id = $1
            )
        )
        "#,
    )
    .bind(entry_id)
    .fetch_one(pool)
    .await
    .expect("应能读取 activation 零写指纹")
}

async fn activate_v3_history(
    state: &AppState,
    bearer: &str,
    entry_id: Uuid,
    publication_id: Uuid,
    base_revision: i64,
    base_lifecycle_revision: i64,
) -> (StatusCode, Value) {
    let idempotency_key = Uuid::now_v7();
    let path = format!("{ROOT}/entries/{entry_id}/publications/{publication_id}/activate");
    let mut body = json!({
        "schema_version": 3,
        "base_revision": base_revision,
        "base_lifecycle_revision": base_lifecycle_revision,
    });
    let (status, response) = call(
        state,
        Method::POST,
        &path,
        bearer,
        Some(idempotency_key),
        Some(body.clone()),
    )
    .await;
    if status != StatusCode::CONFLICT
        || response["code"] != "surface_match_acknowledgement_required"
    {
        return (status, response);
    }
    body["confirmed_surface_match_token"] =
        response["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
    call(
        state,
        Method::POST,
        &path,
        bearer,
        Some(idempotency_key),
        Some(body),
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
async fn republish_after_rollback_reactivates_the_matching_publication(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let draft = create_ready_draft(&state, &pool, &bearer, "rollback-republish").await;
    let (status, first_published) = publish_ready(&state, &bearer, &draft).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "首次发布失败：{first_published}"
    );
    let entry_id = Uuid::parse_str(first_published["word"]["id"].as_str().unwrap()).unwrap();
    let first_revision = first_published["word"]["revision"].as_i64().unwrap();

    let (status, edited) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": first_revision,
            "intent": "complete",
            "content": first_published["word"]["meanings"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "推进 revision 失败：{edited}");
    let (status, second_published) = publish_ready(&state, &bearer, &edited).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "第二次发布失败：{second_published}"
    );
    let second_revision = second_published["word"]["revision"].as_i64().unwrap();
    assert!(second_revision > first_revision);

    let publications: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM lexicon.entry_publications WHERE entry_id = $1 ORDER BY publication_number",
    )
    .bind(entry_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(publications.len(), 2);
    let first_publication_id = publications[0];
    let second_publication_id = publications[1];

    let (status, rolled_back) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications/{first_publication_id}/activate"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": second_revision,
            "base_lifecycle_revision": second_published["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "回滚到历史发布失败：{rolled_back}");
    // 回滚只换 current publication，草稿 revision 原地不动，于是派生出「有未发布改动」。
    assert_eq!(rolled_back["word"]["revision"], second_revision);
    assert_eq!(rolled_back["word"]["published_revision"], first_revision);
    assert_eq!(rolled_back["word"]["has_unpublished_changes"], json!(true));
    let rolled_back_lifecycle_revision =
        rolled_back["word"]["lifecycle_revision"].as_i64().unwrap();
    assert_eq!(
        live_surface_publication_ids(&pool, entry_id).await,
        vec![first_publication_id],
        "回滚后 surface 投影应绑在 pub#1 上"
    );

    // 前端据此显示发布按钮，管理员再点一次发布——草稿内容与 pub#2 完全一致。
    let publish_key = Uuid::now_v7();
    let publish_body = json!({"base_revision": rolled_back["word"]["revision"]});
    let (status, republished) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(publish_key),
        Some(publish_body.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "回滚后重新发布应成功：{republished}"
    );
    assert_eq!(republished["word"]["published_revision"], second_revision);
    assert_eq!(republished["word"]["has_unpublished_changes"], json!(false));
    // 换 current publication 即是一次生命周期变更，必须推进 lifecycle revision。
    assert_eq!(
        republished["word"]["lifecycle_revision"],
        rolled_back_lifecycle_revision + 1
    );

    // 幂等记录写在 publish 作用域下，同一 Idempotency-Key 重放才能原样命中。
    let (status, replayed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(publish_key),
        Some(publish_body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "重放发布失败：{replayed}");
    assert_eq!(replayed, republished);

    // 对外可见数据真正改变的一步：surface 投影必须跟着切回 pub#2。
    assert_eq!(
        live_surface_publication_ids(&pool, entry_id).await,
        vec![second_publication_id],
        "重新发布后 surface 投影应跟着切回 pub#2"
    );

    // 草稿没有变化，不该凭空多出一条 publication；应当重新激活 pub#2。
    let current_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current_publication_id, second_publication_id);
    let publication_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publications WHERE entry_id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(publication_count, 2);

    // 下游消费者靠 publication_activated 感知 current publication 变化，重新发布也要发；
    // 重放不能再发一条。
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
    assert_eq!(
        activation_events,
        vec![
            rolled_back_lifecycle_revision,
            rolled_back_lifecycle_revision + 1
        ]
    );
}

#[sqlx::test]
async fn publish_after_rollback_and_edit_creates_a_new_publication(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let draft = create_ready_draft(&state, &pool, &bearer, "rollback-then-edit").await;
    let (status, first_published) = publish_ready(&state, &bearer, &draft).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "首次发布失败：{first_published}"
    );
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
    assert_eq!(status, StatusCode::OK, "推进 revision 失败：{edited}");
    let (status, second_published) = publish_ready(&state, &bearer, &edited).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "第二次发布失败：{second_published}"
    );

    let first_publication_id: Uuid = sqlx::query_scalar(
        r#"
        SELECT id FROM lexicon.entry_publications
        WHERE entry_id = $1 ORDER BY publication_number LIMIT 1
        "#,
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let (status, rolled_back) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications/{first_publication_id}/activate"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": second_published["word"]["revision"],
            "base_lifecycle_revision": second_published["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "回滚到历史发布失败：{rolled_back}");

    // 回滚后继续改稿，revision 推到一个还没有 publication 的位置，走的仍是新建路径。
    let (status, edited_again) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": rolled_back["word"]["revision"],
            "intent": "complete",
            "content": rolled_back["word"]["meanings"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "回滚后改稿失败：{edited_again}");
    let (status, third_published) = publish_ready(&state, &bearer, &edited_again).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "回滚后改稿再发布失败：{third_published}"
    );
    assert_eq!(
        third_published["word"]["published_revision"],
        edited_again["word"]["revision"]
    );

    let publication_revisions: Vec<i64> = sqlx::query_scalar(
        r#"
        SELECT source_revision FROM lexicon.entry_publications
        WHERE entry_id = $1 ORDER BY publication_number
        "#,
    )
    .bind(entry_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(publication_revisions.len(), 3);
    assert_eq!(
        publication_revisions[2],
        edited_again["word"]["revision"].as_i64().unwrap()
    );
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
async fn saving_meanings_after_backfill_keeps_surface_parity_ready(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let draft = create_ready_draft(&state, &pool, &bearer, "workspace").await;

    let backfill = run_surface_backfill(&pool).await.unwrap();
    assert!(backfill.parity.ready, "backfill 后必须零差异: {backfill:?}");

    let saved = save_example_sentence(&state, &bearer, &draft, "Updated workspace example.").await;
    assert_eq!(
        saved["word"]["revision"].as_i64().unwrap(),
        draft["word"]["revision"].as_i64().unwrap() + 1
    );

    let parity = run_surface_parity(&pool).await.unwrap();
    assert!(
        parity.ready,
        "仅保存 meanings 后 surface parity 必须保持零差异: {parity:?}"
    );
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

    policies
        .transition_exact_headword_creation(&pool, true)
        .await
        .unwrap();
    let (status, post_cutover_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "workspace"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{post_cutover_detection}");
    let confirmation_token = post_cutover_detection["smart_dictionary"]
        ["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .expect("cutover 后命中已有原形必须允许显式确认");
    let (status, second_workspace) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": post_cutover_detection["detection_id"],
            "headwords": post_cutover_detection["builtin_dictionary"]["headwords"],
            "confirmed_surface_match_token": confirmation_token,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second_workspace}");
    let workspace_entries: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT entry_id) FROM lexicon.entry_headword_keys WHERE normalized_headword = 'workspace'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(workspace_entries, 2, "相同原形必须可分别绑定到两个词条");
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
    assert_eq!(detection["schema_version"], 2);
    let decoded_detection: DetectWordResponseV2 = serde_json::from_value(detection.clone())
        .expect("真实 V2 detection response 应按 literal 2 解码");
    assert_eq!(
        serde_json::to_value(&decoded_detection).unwrap()["schema_version"],
        2
    );
    let mut missing_response_version = detection.clone();
    missing_response_version
        .as_object_mut()
        .unwrap()
        .remove("schema_version");
    assert!(
        serde_json::from_value::<DetectWordResponseV2>(missing_response_version).is_err(),
        "response 省略 discriminator 必须 fail closed"
    );
    let mut wrong_response_version = detection.clone();
    wrong_response_version["schema_version"] = json!(3);
    assert!(
        serde_json::from_value::<DetectWordResponseV2>(wrong_response_version).is_err(),
        "V2 response 必须拒绝非 literal 2"
    );
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
    assert_eq!(
        plural_detection["builtin_dictionary"]["status"],
        "not_found"
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
        "headwords": {"mode": "unified", "common": plural_headword},
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

    let unmatched_word = format!("unlisted{}", Uuid::now_v7().simple());
    let (status, unmatched_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": unmatched_word})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        unmatched_detection["builtin_dictionary"]["status"],
        "not_found"
    );
    assert_eq!(unmatched_detection["smart_dictionary"]["status"], "clear");
    let (status, unmatched_create) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": unmatched_detection["detection_id"],
            "headwords": {"mode": "unified", "common": unmatched_word},
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "未收录单词建稿失败：{unmatched_create}"
    );
    assert_eq!(unmatched_create["word"]["kind"], "word");
    assert_eq!(unmatched_create["word"]["forms"], json!({"pos": []}));
    assert_eq!(
        unmatched_create["word"]["detection_snapshot"]["builtin_dictionary_status"],
        "not_found"
    );

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
async fn draft_source_relation_is_included_in_detection_context_with_status(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_headword = format!("meadow{}", admin_id.simple());
    let target = create_ready_draft(&state, &pool, &bearer, &target_headword).await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    let target_sense_id = target["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();
    let (status, target) = publish_ready(&state, &bearer, &target).await;
    assert_eq!(status, StatusCode::CREATED, "目标词发布失败：{target}");

    let source_headword = format!("clear{}", admin_id.simple());
    let source = create_ready_draft(&state, &pool, &bearer, &source_headword).await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": target_entry_id,
        "target_sense_id": target_sense_id,
        "score": "90.00"
    }]);
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "草稿来源关联保存失败：{saved}");

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": target_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "目标词重复检测失败：{detection}");
    let context = detection["smart_dictionary"]["surface_match_page"]["matched_entry_contexts"]
        .as_array()
        .and_then(|contexts| {
            contexts
                .iter()
                .find(|context| context["word_id"] == Value::String(target_entry_id.to_string()))
        })
        .expect("命中目标词时必须返回目标词条上下文");
    assert_eq!(
        context["inbound_relations"]["total"], 1,
        "草稿来源 relation 必须进入检测上下文：{detection}"
    );
    assert_eq!(
        context["inbound_relations"]["previews"][0],
        json!({
            "source_word_id": source_entry_id,
            "source_headword": source_headword,
            "source_status": "draft",
            "relation": "synonym"
        })
    );
}

#[sqlx::test]
async fn legacy_duplicate_fallback_carries_the_inbound_relation_context(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_headword = format!("hollow{}", admin_id.simple());
    let target = create_ready_draft(&state, &pool, &bearer, &target_headword).await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    let target_sense_id = target["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();
    let (status, published) = publish_ready(&state, &bearer, &target).await;
    assert_eq!(status, StatusCode::CREATED, "目标词发布失败：{published}");

    let source_headword = format!("cavity{}", admin_id.simple());
    let source = create_ready_draft(&state, &pool, &bearer, &source_headword).await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": target_entry_id,
        "target_sense_id": target_sense_id,
        "score": "90.00"
    }]);
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "草稿来源关联保存失败：{saved}");

    // 把目标词的词头投影打成 tombstone：legacy exact 索引还在、投影缺了，
    // 检测就落到 duplicate 回退——这条路径没有 surface_match_page 可挂上下文。
    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET is_deleted = TRUE, source_revision = 999999,
            event_offset = nextval('lexicon.surface_projection_event_offset_seq')
        WHERE entry_id = $1 AND source_kind = 'headword'
        "#,
    )
    .bind(target_entry_id)
    .execute(&pool)
    .await
    .unwrap();
    let retired_dataset_id: i64 = sqlx::query_scalar(
        r#"
        INSERT INTO dictionary.datasets (
            version, source_name, source_version, rules_version,
            terms_sha256, regions_sha256, status
        ) VALUES (
            'retired-child-fixture', 'retired', 'old', 'old',
            'old-terms', 'old-regions', 'retired'
        ) RETURNING id
        "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.entry_contents (
            dataset_id, source_key, normalized_term, pos, senses,
            forms, sounds, source_locator
        ) VALUES (
            $1, 'kaikki:child:noun:retired', 'child', 'noun', '[]'::jsonb,
            $2, '[]'::jsonb, 'https://retired.example/source'
        )
        "#,
    )
    .bind(retired_dataset_id)
    .bind(json!([{"form": "childs", "tags": ["plural"]}]))
    .execute(&pool)
    .await
    .unwrap();

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": target_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "目标词重复检测失败：{detection}");
    assert_eq!(
        detection["smart_dictionary"]["status"], "duplicate",
        "词头投影缺失时必须落到 legacy 回退：{detection}"
    );
    let duplicate = detection["smart_dictionary"]["duplicates"]
        .as_array()
        .and_then(|duplicates| {
            duplicates
                .iter()
                .find(|item| item["word_id"] == Value::String(target_entry_id.to_string()))
        })
        .expect("legacy 回退必须返回命中的词条");
    assert_eq!(duplicate["match_category"], "exact_headword");
    assert_eq!(
        duplicate["inbound_relations"]["total"], 1,
        "回退路径也要说出谁在引用这条词条，否则空壳词条会被当成「已经有人建过了」：{detection}"
    );
    assert_eq!(
        duplicate["inbound_relations"]["by_type"],
        json!({"synonym": 1, "antonym": 0, "derivative": 0})
    );
    assert_eq!(duplicate["inbound_relations"]["truncated"], false);
    assert_eq!(
        duplicate["inbound_relations"]["previews"][0],
        json!({
            "source_word_id": source_entry_id,
            "source_headword": source_headword,
            "source_status": "draft",
            "relation": "synonym"
        })
    );
}

#[sqlx::test]
async fn legacy_duplicate_fallback_repeats_the_relation_context_on_every_dialect_row(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // distinguish 词条：uk / us 两个规范化词头各占一条 entry_headword_keys，
    // legacy 回退因此会为同一个词条返回两行。
    let uk_headword = format!("centre{}", admin_id.simple());
    let us_headword = format!("center{}", admin_id.simple());
    seed_dictionary_term(&pool, &us_headword, "word", "british_american").await;
    seed_dictionary_term(&pool, &uk_headword, "word", "british_core").await;
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
            ($1, $2, $2, 'british_american', ARRAY['british_core', 'american_core'],
             ARRAY['GB', 'US'], ARRAY['spelling'], ARRAY['noun'], ARRAY[$3], true),
            ($1, $3, $3, 'british_core', ARRAY['british_core'],
             ARRAY['GB'], ARRAY['spelling'], ARRAY['noun'], ARRAY[$2], true)
        "#,
    )
    .bind(dataset_id)
    .bind(&us_headword)
    .bind(&uk_headword)
    .execute(&pool)
    .await
    .unwrap();

    let target = create_ready_draft(&state, &pool, &bearer, &us_headword).await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(
        target["word"]["headwords"]["mode"], "distinguish",
        "种子数据应当产出 distinguish 词条：{target}"
    );
    let target_sense_id = target["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();
    let (status, published) = publish_ready(&state, &bearer, &target).await;
    assert_eq!(status, StatusCode::CREATED, "目标词发布失败：{published}");

    let source_headword = format!("midpoint{}", admin_id.simple());
    let source = create_ready_draft(&state, &pool, &bearer, &source_headword).await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": target_entry_id,
        "target_sense_id": target_sense_id,
        "score": "90.00"
    }]);
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "草稿来源关联保存失败：{saved}");

    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET is_deleted = TRUE, source_revision = 999999,
            event_offset = nextval('lexicon.surface_projection_event_offset_seq')
        WHERE entry_id = $1 AND source_kind = 'headword'
        "#,
    )
    .bind(target_entry_id)
    .execute(&pool)
    .await
    .unwrap();

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": us_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "目标词重复检测失败：{detection}");
    assert_eq!(detection["smart_dictionary"]["status"], "duplicate");
    let rows = detection["smart_dictionary"]["duplicates"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["word_id"] == Value::String(target_entry_id.to_string()))
        .collect::<Vec<_>>();
    assert_eq!(
        rows.len(),
        2,
        "distinguish 词条的 uk / us 两个词头键应当各出一行：{detection}"
    );
    // 摘要按词条聚合，与撞上的是哪一侧词头无关——两行必须给出同一份被引用上下文，
    // 否则前端会在其中一行提示「被引用」、另一行什么都不提示。
    for row in rows {
        assert_eq!(
            row["inbound_relations"]["total"], 1,
            "每一行都要带上被引用上下文：{detection}"
        );
        assert_eq!(
            row["inbound_relations"]["previews"][0],
            json!({
                "source_word_id": source_entry_id,
                "source_headword": source_headword,
                "source_status": "draft",
                "relation": "synonym"
            })
        );
    }
}

#[sqlx::test]
async fn detection_reports_which_entries_reference_the_matched_surface_as_a_relation(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let policies = state.surface_policy_store_for_test();
    prepare_duplicate_headword_test(&pool, &policies).await;
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 目标词已建条，随后被另一个词条引用为近义词——这正是「录入 pome 时
    // apples 的近义词里已经写了 pome」的场景。
    let target_headword = format!("relhittarget{}", admin_id.simple());
    let target = create_ready_draft(&state, &pool, &bearer, &target_headword).await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    let target_sense_id = target["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();
    let (status, target) = publish_ready(&state, &bearer, &target).await;
    assert_eq!(status, StatusCode::CREATED, "目标发布失败：{target}");

    let source_headword = format!("relhitsource{}", admin_id.simple());
    let source = create_ready_draft(&state, &pool, &bearer, &source_headword).await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let mut meanings = source["word"]["meanings"].clone();
    // 同一个引用方用同一种关系指向同一个目标写两条，命中行必须只出现一次。
    meanings["pos"][0]["senses"][0]["relations"] = json!([
        {
            "id": Uuid::now_v7(),
            "relation": "synonym",
            "target_word_id": target_entry_id,
            "target_sense_id": target_sense_id,
            "score": "90.00"
        },
        {
            "id": Uuid::now_v7(),
            "relation": "antonym",
            "target_word_id": target_entry_id,
            "target_sense_id": target_sense_id,
            "score": "10.00"
        },
        {
            "id": Uuid::now_v7(),
            "relation": "synonym",
            "target_word_id": target_entry_id,
            "target_sense_id": target_sense_id,
            "score": "80.00"
        }
    ]);
    let (status, source_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "来源关联保存失败：{source_saved}");

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": target_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "重复检测失败：{detection}");
    let items = detection["smart_dictionary"]["surface_match_page"]["items"]
        .as_array()
        .expect("命中页应有条目");

    assert!(
        items
            .iter()
            .any(|item| item["match_category"] == "exact_headword"
                && item["existing"]["word_id"] == target_entry_id.to_string()),
        "关联词维度不得取代原有的主词命中：{items:?}"
    );

    let relation_items = items
        .iter()
        .filter(|item| item["match_category"] == "headword_relation")
        .collect::<Vec<_>>();
    assert_eq!(
        relation_items.len(),
        2,
        "两种关系各一行，同一 (引用方, 关系类型) 的重复关联必须去重：{items:?}"
    );
    // 命中行数与 inbound_relations.total 合法地不相等：命中行是「命中原因」，
    // 按 (引用方, 关系类型) 去重；total 数的是真实关联条数。两者都要保持原样。
    let summary = &detection["smart_dictionary"]["surface_match_page"]["matched_entry_contexts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|context| context["word_id"] == target_entry_id.to_string())
        .expect("命中词条必须有上下文")["inbound_relations"];
    assert_eq!(
        summary["total"], 3,
        "total 必须数全部关联，不跟着命中行去重"
    );
    assert_eq!(summary["by_type"]["synonym"], 2);
    assert_eq!(summary["by_type"]["antonym"], 1);
    for item in &relation_items {
        // 命中行归属拥有该词面的词条；引用方是命中原因里的施动者。
        assert_eq!(item["existing"]["word_id"], target_entry_id.to_string());
        assert_eq!(item["existing"]["headword"], target_headword);
        assert_eq!(item["existing"]["source"]["source_kind"], "relation");
        assert_eq!(
            item["existing"]["source"]["referencing_word_id"],
            source_entry_id.to_string()
        );
        assert_eq!(
            item["existing"]["source"]["referencing_headword"],
            source_headword
        );
        assert_eq!(item["existing"]["source"]["referencing_status"], "draft");
        assert_eq!(item["existing"]["source"]["surface"], target_headword);
        assert_eq!(item["attention_level"], "normal");
        assert_eq!(item["can_continue"], true);
    }
    let mut relation_types = relation_items
        .iter()
        .map(|item| {
            item["existing"]["source"]["relation_type"]
                .as_str()
                .unwrap()
        })
        .collect::<Vec<_>>();
    relation_types.sort_unstable();
    assert_eq!(relation_types, ["antonym", "synonym"]);

    // 关联词维度只是补充说明，不得把 policy 从 exact-headword 那一路挪走。
    assert_eq!(
        detection["smart_dictionary"]["surface_match_page"]["policy_name"],
        "allow_new_exact_headword_entries"
    );

    // 没有任何词条引用它时不得凭空产生 relation 命中。
    let (status, source_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": source_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "来源词检测失败：{source_detection}");
    assert!(
        source_detection["smart_dictionary"]["surface_match_page"]["items"]
            .as_array()
            .expect("来源词自身也会命中主词")
            .iter()
            .all(|item| item["match_category"] != "headword_relation"),
        "无入站关联的词面不得出现关联词命中：{source_detection}"
    );
}

#[sqlx::test]
async fn a_third_party_relation_reopens_publish_confirmation_with_a_relation_hit(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let policies = state.surface_policy_store_for_test();
    prepare_duplicate_headword_test(&pool, &policies).await;
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 同名的在位词条留在草稿——发布同名词条只需过词面确认，不去撞「多活主词发布」
    // 那道默认关闭的可见性闸门，测试才盯得住关联词维度本身。
    let shared_headword = format!("relpubshared{}", admin_id.simple());
    let incumbent = create_ready_draft(&state, &pool, &bearer, &shared_headword).await;
    let incumbent_entry_id = Uuid::parse_str(incumbent["word"]["id"].as_str().unwrap()).unwrap();
    let incumbent_sense_id = incumbent["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();

    // 建稿时就确认过当时的词面冲突，此刻还没有任何关联词。
    let challenger = create_ready_draft(&state, &pool, &bearer, &shared_headword).await;

    // 第三方给在位词条挂上一条反义词——这会给同名词条的发布凭空多出一条命中，
    // 让建稿时的确认作废。这是关联词维度带来的行为扩大，钉在这里以免悄悄回归。
    let referencing_headword = format!("relpubsource{}", admin_id.simple());
    let referencing = create_ready_draft(&state, &pool, &bearer, &referencing_headword).await;
    let referencing_entry_id =
        Uuid::parse_str(referencing["word"]["id"].as_str().unwrap()).unwrap();
    let mut meanings = referencing["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "antonym",
        "target_word_id": incumbent_entry_id,
        "target_sense_id": incumbent_sense_id,
        "score": "70.00"
    }]);
    let (status, referencing_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{referencing_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": referencing["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "引用方保存失败：{referencing_saved}"
    );

    let (status, blocked) = publish_ready(&state, &bearer, &challenger).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "第三方新增关联词后必须重新确认：{blocked}"
    );
    assert_eq!(blocked["code"], "surface_match_acknowledgement_required");

    let page = &blocked["meta"]["surface_match_page"];
    let items = page["items"].as_array().expect("确认页应有条目");
    let relation_items = items
        .iter()
        .filter(|item| item["match_category"] == "headword_relation")
        .collect::<Vec<_>>();
    assert_eq!(
        relation_items.len(),
        1,
        "发布确认页也要带关联词命中：{items:?}"
    );
    assert_eq!(
        relation_items[0]["existing"]["word_id"],
        incumbent_entry_id.to_string()
    );
    assert_eq!(
        relation_items[0]["existing"]["source"]["referencing_word_id"],
        referencing_entry_id.to_string()
    );
    assert_eq!(
        relation_items[0]["existing"]["source"]["relation_type"],
        "antonym"
    );
    // 关联词命中不是词面来源，不得被算进「多活主词发布」的可见性判定。
    assert!(
        relation_items[0]["confirmation_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .all(|reason| reason != "visibility_activation"),
        "关联词命中不得进入可见性确认：{items:?}"
    );
    // 快照校验要求每个命中条目都有对应上下文；relation 命中归属在位词条，不新增 word_id。
    let context_ids = page["matched_entry_contexts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|context| context["word_id"].clone())
        .collect::<Vec<_>>();
    assert!(context_ids.contains(&relation_items[0]["existing"]["word_id"]));

    let confirmation_token = page["surface_confirmation_token"]
        .as_str()
        .expect("确认页应签发 token")
        .to_owned();
    let (status, published) = call(
        &state,
        Method::POST,
        &format!(
            "{ROOT}/entries/{}/publications",
            challenger["word"]["id"].as_str().unwrap()
        ),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": challenger["word"]["revision"],
            "confirmed_surface_match_token": confirmation_token,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "确认后必须能发布，关联词命中不得变成死锁：{published}"
    );
    // 关联词命中不得写进永久落库的检测审计——旧二进制没有 headword_relation 这个
    // 取值，写进去会让回退后的实例读不出这条词条。
    let persisted: serde_json::Value =
        sqlx::query_scalar("SELECT detection_snapshot FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(challenger["word"]["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .expect("应能读取检测快照");
    assert!(
        persisted["surface_warning"]["preview"]
            .as_array()
            .expect("审计预览应存在")
            .iter()
            .all(|item| item["match_category"] != "headword_relation"),
        "落库的检测审计不得出现新枚举取值：{persisted}"
    );
}

#[sqlx::test]
async fn detection_confirmation_rejects_changed_inbound_relation_context(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let policies = state.surface_policy_store_for_test();
    prepare_duplicate_headword_test(&pool, &policies).await;
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_headword = format!("tokenmeadow{}", admin_id.simple());
    let target = create_ready_draft(&state, &pool, &bearer, &target_headword).await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    let target_sense_id = target["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();
    let (status, target) = publish_ready(&state, &bearer, &target).await;
    assert_eq!(status, StatusCode::CREATED, "目标发布失败：{target}");

    let source_headword = format!("tokensource{}", admin_id.simple());
    let source = create_ready_draft(&state, &pool, &bearer, &source_headword).await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": target_entry_id,
        "target_sense_id": target_sense_id,
        "score": "90.00"
    }]);
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": target_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "重复检测失败：{detection}");
    let confirmation_token =
        detection["smart_dictionary"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .expect("开启 exact-headword policy 后终页应签发 token")
            .to_owned();

    let mut relation_barrier = pool.begin().await.unwrap();
    LexiconRepository::lock_surface_contexts(&mut relation_barrier, &[target_entry_id])
        .await
        .unwrap();
    let mut unrelated_context = pool.begin().await.unwrap();
    LexiconRepository::lock_surface_contexts(&mut unrelated_context, &[source_entry_id])
        .await
        .expect("不同 entry 的 context 锁不得互相阻塞");
    unrelated_context.rollback().await.unwrap();
    let (status, concurrent_save) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings.clone(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "并发上下文写入应快速失败");
    assert_eq!(concurrent_save["code"], "reference_conflict");
    relation_barrier.rollback().await.unwrap();
    let (status, source_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "来源关联保存失败：{source_saved}");

    let (status, changed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": detection["builtin_dictionary"]["headwords"],
            "confirmed_surface_match_token": confirmation_token
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "旧上下文 token 不得继续创建：{changed}"
    );
    assert_eq!(changed["code"], "surface_matches_changed");
    assert!(
        changed["meta"]["surface_match_page"]["matched_entry_contexts"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|context| context["inbound_relations"]["previews"].as_array().unwrap())
            .any(
                |preview| preview["source_word_id"] == source_entry_id.to_string()
                    && preview["source_status"] == "draft"
            )
    );

    let refreshed_token = changed["meta"]["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .expect("变化后的终页应返回新确认 token")
        .to_owned();

    let mut forms_without_pos = source_saved["word"]["forms"].clone();
    forms_without_pos["pos"] = json!([]);
    let (status, forms_impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{source_entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source_saved["word"]["revision"],
            "content": forms_without_pos.clone(),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "删除来源 POS impact 失败：{forms_impact}"
    );
    assert_eq!(forms_impact["requires_confirmation"], true);
    let mut forms_barrier = pool.begin().await.unwrap();
    LexiconRepository::lock_surface_contexts(&mut forms_barrier, &[target_entry_id])
        .await
        .unwrap();
    let (status, concurrent_forms_save) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source_saved["word"]["revision"],
            "intent": "save",
            "confirmed_impact_token": forms_impact["confirmation_token"],
            "content": forms_without_pos.clone(),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "forms 删除 relation 时必须快速等待目标 context：{concurrent_forms_save}"
    );
    assert_eq!(concurrent_forms_save["code"], "reference_conflict");
    forms_barrier.rollback().await.unwrap();
    let (status, forms_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source_saved["word"]["revision"],
            "intent": "save",
            "confirmed_impact_token": forms_impact["confirmation_token"],
            "content": forms_without_pos,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "删除来源 POS 保存失败：{forms_saved}"
    );
    assert_eq!(forms_saved["word"]["meanings"]["pos"], json!([]));

    let (status, forms_changed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": detection["builtin_dictionary"]["headwords"],
            "confirmed_surface_match_token": refreshed_token
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "forms 删除 relation 后旧 token 必须失效：{forms_changed}"
    );
    assert_eq!(forms_changed["code"], "surface_matches_changed");
    let post_forms_token =
        forms_changed["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .expect("forms 删除 relation 后应签发新确认 token")
            .to_owned();
    let mut consumption_barrier = pool.begin().await.unwrap();
    LexiconRepository::lock_surface_contexts(&mut consumption_barrier, &[target_entry_id])
        .await
        .unwrap();
    let (status, concurrent_consume) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": detection["builtin_dictionary"]["headwords"],
            "confirmed_surface_match_token": post_forms_token.clone()
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "并发 token 消费应快速失败");
    assert_eq!(concurrent_consume["code"], "reference_conflict");
    consumption_barrier.rollback().await.unwrap();
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
            "confirmed_surface_match_token": post_forms_token
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "新 token 创建失败：{created}");

    policies
        .transition_exact_headword_creation(&pool, false)
        .await
        .unwrap();
}

#[sqlx::test]
async fn published_relation_can_target_draft_and_keeps_an_auditable_stable_anchor(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_headword = format!("draftmeadow{}", admin_id.simple());
    let target = create_ready_draft(&state, &pool, &bearer, &target_headword).await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    let target_sense_id = Uuid::parse_str(
        target["word"]["meanings"]["pos"][0]["senses"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let target_revision = target["word"]["revision"].as_i64().unwrap();

    let source_headword = format!("publishedclear{}", admin_id.simple());
    let source = create_ready_draft(&state, &pool, &bearer, &source_headword).await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let relation_id = Uuid::now_v7();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id,
        "relation": "synonym",
        "target_word_id": target_entry_id,
        "target_sense_id": target_sense_id,
        "target_headword": "客户端伪造词头",
        "target_gloss": "客户端伪造释义",
        "score": "95.00"
    }]);
    let (status, source_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "relation 应能保存到草稿目标：{source_saved}"
    );
    let saved_relation = &source_saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(saved_relation["target_headword"], target_headword);
    assert_eq!(
        saved_relation["target_gloss"],
        format!("{target_headword} 的释义"),
        "只读快照字段必须由服务端覆盖"
    );

    let (status, validation) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{source_entry_id}/validate"),
        &bearer,
        None,
        Some(json!({"base_revision": source_saved["word"]["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "草稿目标关联校验失败：{validation}");
    assert_eq!(validation["valid"], true);

    let (status, source_published) = publish_ready(&state, &bearer, &source_saved).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "不得要求目标先发布才能发布来源关联：{source_published}"
    );
    let source_revision = source_published["word"]["revision"].as_i64().unwrap();
    let source_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(source_entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let anchor: (Option<Uuid>, String, i64) = sqlx::query_as(
        r#"
        SELECT target_publication_id, target_content_scope, target_revision
        FROM lexicon.entry_publication_sense_refs
        WHERE publication_id = $1 AND source_node_id = $2
        "#,
    )
    .bind(source_publication_id)
    .bind(relation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(anchor, (None, "draft".to_owned(), target_revision));
    let publication_snapshot: Value =
        sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id = $1")
            .bind(source_publication_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let snapshotted_relation =
        &publication_snapshot["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(snapshotted_relation["target_headword"], target_headword);
    assert_eq!(
        snapshotted_relation["target_sense_id"],
        json!(target_sense_id)
    );

    let (status, blocked_delete) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{target_entry_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": target_revision,
            "base_lifecycle_revision": 1
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "正式历史引用必须阻止硬删除");
    assert_eq!(blocked_delete["code"], "entry_not_deletable");

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
    assert_eq!(status, StatusCode::CONFLICT, "当前正式入链必须阻止目标归档");
    assert_eq!(
        blocked_archive["code"],
        "entry_has_inbound_publication_refs"
    );

    let (status, target_published) = publish_ready(&state, &bearer, &target).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "目标后续发布失败：{target_published}"
    );
    let anchor_after_target_publish: (Option<Uuid>, String, i64) = sqlx::query_as(
        r#"
        SELECT target_publication_id, target_content_scope, target_revision
        FROM lexicon.entry_publication_sense_refs
        WHERE publication_id = $1 AND source_node_id = $2
        "#,
    )
    .bind(source_publication_id)
    .bind(relation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        anchor_after_target_publish, anchor,
        "目标后续发布不得回写历史来源 publication 的审计锚点"
    );

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": target_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "目标发布后检测失败：{detection}");
    let previews = detection["smart_dictionary"]["surface_match_page"]["matched_entry_contexts"][0]
        ["inbound_relations"]["previews"]
        .as_array()
        .unwrap();
    assert_eq!(
        previews.len(),
        1,
        "draft/current publication 同一关联不得重复计数"
    );
    assert_eq!(previews[0]["source_status"], "published");

    let (status, source_archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{source_entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": source_revision,
            "base_lifecycle_revision": 1
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "来源归档失败：{source_archived}");
    let (status, archived_source_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": target_headword})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "来源归档后目标检测失败：{archived_source_detection}"
    );
    assert!(archived_source_detection["smart_dictionary"]["surface_match_page"]
        ["matched_entry_contexts"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|context| context["inbound_relations"]["previews"].as_array().unwrap())
        .any(|preview| preview["source_word_id"] == source_entry_id.to_string()
            && preview["source_status"] == "archived"));
    let target_published_revision = target_published["word"]["revision"].as_i64().unwrap();

    let mut target_write = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM lexicon.entries WHERE id = $1 FOR UPDATE")
        .bind(target_entry_id)
        .fetch_one(&mut *target_write)
        .await
        .unwrap();
    let (status, concurrent_restore) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{source_entry_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": source_revision,
            "base_lifecycle_revision": 2
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "恢复必须锁定正式出链目标，不能越过并发目标写入：{concurrent_restore}"
    );
    assert_eq!(concurrent_restore["code"], "reference_conflict");
    target_write.rollback().await.unwrap();

    let (status, target_archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": target_published_revision,
            "base_lifecycle_revision": 1
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "来源归档后目标应可归档：{target_archived}"
    );
    let (status, blocked_restore) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{source_entry_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": source_revision,
            "base_lifecycle_revision": 2
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "目标归档时不得恢复来源：{blocked_restore}"
    );
    assert_eq!(
        blocked_restore["code"],
        "entry_has_unavailable_publication_refs"
    );
}

#[sqlx::test]
async fn sentence_context_still_requires_a_published_target(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("contextdraft{}", admin_id.simple()),
    )
    .await;
    let target_entry_id = target["word"]["id"].clone();
    let target_sense_id = target["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();
    let source = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("contextsource{}", admin_id.simple()),
    )
    .await;
    let source_entry_id = source["word"]["id"].as_str().unwrap();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["sentences"][0]["links"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "word_id": target_entry_id,
            "sense_id": target_sense_id,
            "role": "context"
        }));
    let (status, rejected) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert!(has_issue(&rejected, "sentence_context_target_unavailable"));
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
                "schema_version": 2,
                "word_id": word_id,
                "headword": "colour",
                "kind": "word",
                "dialects": ["common"],
                "headword_variants": [{"dialect": "common", "headword": "colour"}],
                "pos_labels": [],
                "senses": [{"sense_id": sense_id, "gloss": "颜色"}]
            }, {
                "schema_version": 2,
                "word_id": second_word_id,
                "headword": "colour",
                "kind": "word",
                "dialects": ["common"],
                "headword_variants": [{"dialect": "common", "headword": "colour"}],
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
                "schema_version": 2,
                "word_id": phrase_id,
                "headword": "colour centre",
                "kind": "phrase",
                "dialects": ["common"],
                "headword_variants": [{"dialect": "common", "headword": "colour centre"}],
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

/// 内置词典是 Kaikki 静态快照，未命中不代表词不存在（品牌名、新造词、缩写都在外面），
/// 单词必须和短语一样能人工建稿，只是没有任何词典建议可继承。
#[sqlx::test]
async fn unmatched_word_creates_a_manual_draft_without_dictionary_suggestions(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let headword = format!("brandnew{}", Uuid::now_v7().simple());

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "未收录单词检测失败：{detection}");
    assert_eq!(detection["entry_kind"], "word");
    assert_eq!(detection["builtin_dictionary"]["status"], "not_found");
    assert_eq!(detection["smart_dictionary"]["status"], "clear");

    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {"mode": "unified", "common": headword},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "未收录单词建稿失败：{created}");
    assert_eq!(created["word"]["kind"], "word");
    assert_eq!(created["word"]["status"], "draft");
    assert_eq!(
        created["word"]["headwords"],
        json!({"mode": "unified", "common": headword})
    );
    assert_eq!(created["word"]["forms"], json!({"pos": []}));
    assert_eq!(created["word"]["meanings"]["pos"], json!([]));
    assert_eq!(created["word"]["max_reachable_step"], "forms");
    assert_eq!(
        created["word"]["detection_snapshot"]["builtin_dictionary_status"],
        "not_found"
    );
    assert_eq!(
        created["word"]["detection_snapshot"]["smart_dictionary_status"],
        "clear"
    );
    assert_eq!(created["word"]["detection_snapshot"]["entry_kind"], "word");
    assert_eq!(
        created["word"]["detection_snapshot"]["matched_dialect"],
        "common"
    );
    assert_eq!(
        created["word"]["detection_snapshot"]["suggested_pos"],
        json!([])
    );

    // 未命中时词头没有词典来源，必须记成人工来源，并照常进入表层投影。
    let entry_id = Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap();
    let origins: Vec<String> =
        sqlx::query_scalar("SELECT origin FROM lexicon.entry_headwords WHERE entry_id = $1")
            .bind(entry_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(origins, ["manual"]);
    let projected: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT dialect_scope FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_scope = 'draft'
          AND source_kind = 'headword' AND is_deleted = FALSE
        ORDER BY dialect_scope
        "#,
    )
    .bind(entry_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(projected, ["uk", "us"]);
}

/// 未命中放开的只是「词典没收录」这一条闸：重复、词典临时不可用、词头对不上仍要拒。
#[sqlx::test]
async fn unmatched_word_creation_still_rejects_duplicates_unavailable_and_mismatched_headwords(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis.clone());
    let detections = DetectionStore::new(redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let duplicated = format!("brandnew{}", Uuid::now_v7().simple());
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": duplicated})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "未收录单词检测失败：{detection}");
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {"mode": "unified", "common": duplicated},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "未收录单词建稿失败：{created}");

    // 把已建词条的词头投影打成 tombstone：legacy exact 索引仍在但投影已缺，
    // 这正是 smart_dictionary = duplicate 的成因。
    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET is_deleted = TRUE, source_revision = 999999,
            event_offset = nextval('lexicon.surface_projection_event_offset_seq')
        WHERE entry_id = $1 AND source_kind = 'headword'
        "#,
    )
    .bind(Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap())
    .execute(&pool)
    .await
    .unwrap();

    let (status, duplicate_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": duplicated})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        duplicate_detection["builtin_dictionary"]["status"],
        "not_found"
    );
    assert_eq!(
        duplicate_detection["smart_dictionary"]["status"],
        "duplicate"
    );
    // duplicate 这一路也要带命中原因，前端才不用靠分支猜。legacy 精确主词索引是
    // 它唯一的触发来源，所以类别恒为 exact_headword。
    let duplicates = duplicate_detection["smart_dictionary"]["duplicates"]
        .as_array()
        .expect("duplicate 分支必须列出重复词条");
    assert!(!duplicates.is_empty());
    assert!(
        duplicates
            .iter()
            .all(|item| item["match_category"] == "exact_headword"),
        "duplicate 与 warning 两条路径的信息量必须一致：{duplicates:?}"
    );
    let (status, rejected) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": duplicate_detection["detection_id"],
            "headwords": {"mode": "unified", "common": duplicated},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "重复词必须拒绝：{rejected}");
    assert_eq!(rejected["code"], "duplicate_word");

    // 词典临时不可用是故障，应当重试而不是当成「未收录」绕过。detect 目前不会产出
    // unavailable，所以直接改写检测上下文，把这条契约钉在创建侧。
    let unavailable_headword = format!("brandnew{}", Uuid::now_v7().simple());
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": unavailable_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "未收录单词检测失败：{detection}");
    let mut unavailable = detection.clone();
    unavailable["builtin_dictionary"] = json!({"status": "unavailable"});
    let unavailable: DetectWordResponseV2 =
        serde_json::from_value(unavailable).expect("改写后的检测上下文应能反序列化");
    detections
        .save(admin_id, &unavailable, std::time::Duration::from_secs(300))
        .await
        .expect("覆盖检测上下文应成功");
    let (status, rejected) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {"mode": "unified", "common": unavailable_headword},
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "词典不可用不得当成未收录放行：{rejected}"
    );
    assert_eq!(rejected["code"], "detection_mismatch");

    // 未命中只签发统一词头，提交区分词形或换掉词头都属于凭据不符。
    let tampered = format!("brandnew{}", Uuid::now_v7().simple());
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": tampered})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "未收录单词检测失败：{detection}");
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
                "uk": format!("{tampered}-uk"),
                "us": format!("{tampered}-us"),
                "source_dialect": "uk"
            },
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "未命中不得提交区分词形：{rejected}"
    );
    assert_eq!(rejected["code"], "detection_mismatch");

    let (status, rejected) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {"mode": "unified", "common": format!("{tampered}x")},
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "词头被换掉必须拒绝：{rejected}"
    );
    assert_eq!(rejected["code"], "detection_mismatch");
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
    let mut row_barrier = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM lexicon.entries WHERE id = $1 FOR UPDATE")
        .bind(entry_uuid)
        .fetch_one(&mut *row_barrier)
        .await
        .unwrap();
    let first_state = state.clone();
    let first_uri = archive_uri.clone();
    let first_bearer = bearer.clone();
    let first_body = double_click_body.clone();
    let first = tokio::spawn(async move {
        call(
            &first_state,
            Method::POST,
            &first_uri,
            &first_bearer,
            Some(Uuid::now_v7()),
            Some(first_body),
        )
        .await
    });
    let second_state = state.clone();
    let second_bearer = bearer.clone();
    let second = tokio::spawn(async move {
        call(
            &second_state,
            Method::POST,
            &archive_uri,
            &second_bearer,
            Some(Uuid::now_v7()),
            Some(double_click_body),
        )
        .await
    });
    await_database_lock_waiters(&pool, 2).await;
    assert!(!first.is_finished() && !second.is_finished());
    row_barrier.commit().await.unwrap();
    let first = tokio::time::timeout(CONCURRENCY_TIMEOUT, first)
        .await
        .expect("释放行锁后首次归档应完成")
        .unwrap();
    let second = tokio::time::timeout(CONCURRENCY_TIMEOUT, second)
        .await
        .expect("释放行锁后第二次归档应完成")
        .unwrap();
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

    sqlx::query(
        r#"
        INSERT INTO dictionary.region_evidence (
            dataset_id, normalized_term, evidence_type,
            original_region_tags, raw_tags, pos, targets
        ) VALUES
            ($1, 'center', 'spelling', ARRAY['US'], ARRAY['US', 'alternative'],
             'noun', ARRAY['centre']),
            ($1, 'centre', 'spelling', ARRAY['UK'], ARRAY['UK', 'alternative'],
             'noun', ARRAY['center'])
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

    let v3_state = state
        .clone()
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    for input in ["center", "centre"] {
        let (status, response) = call(
            &v3_state,
            Method::POST,
            &format!("{ROOT}/detections"),
            &bearer,
            None,
            Some(json!({
                "schema_version": 3,
                "language": "en",
                "kind": "word",
                "surface": input
            })),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{input} V3 检测失败：{response}");
        assert_eq!(
            response["builtin_dictionary"]["suggested_forms"][0]["regional_variants"],
            json!({
                "mode": "uk_us",
                "uk": {"dialect": "uk", "spelling": "centre", "pronunciations": []},
                "us": {"dialect": "us", "spelling": "center", "pronunciations": []}
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
        ) VALUES
            ($1, 'priority-source', 'priority-source', 'british_core', ARRAY['british_core'],
             ARRAY['GB'], ARRAY['spelling'], ARRAY['noun'],
             ARRAY['priority-ambiguous', 'priority-us'], true),
            ($1, 'priority-ambiguous', 'priority-ambiguous', 'british_american',
             ARRAY['british_core', 'american_core'], ARRAY['GB', 'US'], ARRAY['usage'],
             ARRAY['noun'], ARRAY[]::TEXT[], true),
            ($1, 'priority-us', 'priority-us', 'american_core', ARRAY['american_core'],
             ARRAY['US'], ARRAY['usage'], ARRAY['noun'], ARRAY[]::TEXT[], true)
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.region_evidence (
            dataset_id, normalized_term, evidence_type,
            original_region_tags, raw_tags, pos, targets
        ) VALUES (
            $1, 'priority-source', 'spelling', ARRAY['UK'], ARRAY['UK', 'alternative'],
            'noun', ARRAY['priority-ambiguous', 'priority-us']
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
    let (status, v3_priority) = call(
        &v3_state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "priority-source"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v3_priority}");
    assert_eq!(
        v3_priority["builtin_dictionary"]["suggested_forms"][0]["regional_variants"],
        json!({
            "mode": "uk_us",
            "uk": {"dialect": "uk", "spelling": "priority-source", "pronunciations": []},
            "us": {"dialect": "us", "spelling": "priority-us", "pronunciations": []}
        })
    );
}

#[sqlx::test]
async fn v3_detection_recovers_an_asymmetric_region_index_in_both_directions(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    seed_dictionary_term(&pool, "metreprobe", "word", "british_core").await;
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
            ($1, 'a-metreprobe-inflected', 'a-metreprobe-inflected', 'american_core',
             ARRAY['american_core'], ARRAY['US'], ARRAY['spelling'], ARRAY['noun'],
             ARRAY['metreprobe'], false),
            ($1, 'meterprobe', 'meterprobe', 'american_core', ARRAY['american_core'],
             ARRAY['US'], ARRAY['spelling'], ARRAY['noun'], ARRAY['metreprobe'], true),
            ($1, 'metreprobe', 'metreprobe', 'british_core', ARRAY['british_core'],
             ARRAY['GB'], ARRAY['usage'], ARRAY['noun'], ARRAY[]::TEXT[], true)
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.region_evidence (
            dataset_id, normalized_term, evidence_type,
            original_region_tags, raw_tags, pos, targets
        ) VALUES
            ($1, 'a-metreprobe-inflected', 'spelling', ARRAY['US'],
             ARRAY['US', 'past', 'participle'], 'noun', ARRAY['metreprobe']),
            ($1, 'meterprobe', 'spelling', ARRAY['US'], ARRAY['US', 'alternative'],
             'noun', ARRAY['metreprobe'])
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();

    for input in ["meterprobe", "metreprobe"] {
        let (status, response) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/detections"),
            &bearer,
            None,
            Some(json!({
                "schema_version": 3,
                "language": "en",
                "kind": "word",
                "surface": input
            })),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{input} V3 检测失败：{response}");
        assert_eq!(response["builtin_dictionary"]["status"], "matched");
        assert_eq!(
            response["builtin_dictionary"]["suggested_forms"][0]["regional_variants"],
            json!({
                "mode": "uk_us",
                "uk": {"dialect": "uk", "spelling": "metreprobe", "pronunciations": []},
                "us": {"dialect": "us", "spelling": "meterprobe", "pronunciations": []}
            })
        );
    }

    seed_dictionary_term(&pool, "plural-probe", "word", "american_core").await;
    seed_dictionary_term(&pool, "singular-probe", "word", "common_unmarked").await;
    seed_dictionary_term(&pool, "unrelated-spelling-probe", "word", "british_core").await;
    sqlx::query(
        r#"
        INSERT INTO dictionary.region_surfaces (
            dataset_id, normalized_term, term, region_family, families,
            source_regions, evidence_types, pos, targets, is_headword
        ) VALUES
            ($1, 'plural-probe', 'plural-probe', 'american_core', ARRAY['american_core'],
             ARRAY['US'], ARRAY['alias', 'spelling', 'usage'], ARRAY['noun'],
             ARRAY['singular-probe', 'unrelated-spelling-probe'], true),
            ($1, 'singular-probe', 'singular-probe', 'british_core', ARRAY['british_core'],
             ARRAY['UK'], ARRAY['usage'], ARRAY['noun'], ARRAY[]::TEXT[], true),
            ($1, 'unrelated-spelling-probe', 'unrelated-spelling-probe', 'british_core',
             ARRAY['british_core'], ARRAY['UK'], ARRAY['usage'], ARRAY['noun'],
             ARRAY[]::TEXT[], true)
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.region_evidence (
            dataset_id, normalized_term, evidence_type,
            original_region_tags, raw_tags, pos, targets
        ) VALUES
        (
            $1, 'plural-probe', 'alias', ARRAY['US'], ARRAY['US', 'alt-of'],
            'noun', ARRAY['singular-probe']
        ),
        (
            $1, 'plural-probe', 'spelling', ARRAY['US'],
            ARRAY['US', 'abbreviation', 'alternative'], 'noun',
            ARRAY['unrelated-spelling-probe']
        )
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();
    let (status, unrelated_alias) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "plural-probe"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unrelated_alias}");
    assert_eq!(
        unrelated_alias["builtin_dictionary"]["suggested_forms"][0]["regional_variants"],
        json!({
            "mode": "common",
            "common": {
                "dialect": "common",
                "spelling": "plural-probe",
                "pronunciations": []
            }
        })
    );

    seed_dictionary_term(&pool, "local-probe", "word", "british_core").await;
    seed_dictionary_term(&pool, "local-variant-probe", "word", "american_core").await;
    sqlx::query(
        r#"
        INSERT INTO dictionary.region_surfaces (
            dataset_id, normalized_term, term, region_family, families,
            source_regions, evidence_types, pos, targets, is_headword
        ) VALUES
            ($1, 'local-probe', 'local-probe', 'british_core', ARRAY['british_core'],
             ARRAY['UK'], ARRAY['spelling'], ARRAY['noun'], ARRAY['local-variant-probe'], true),
            ($1, 'local-variant-probe', 'local-variant-probe', 'american_core',
             ARRAY['american_core'], ARRAY['US'], ARRAY['usage'], ARRAY['noun'],
             ARRAY[]::TEXT[], true)
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.region_evidence (
            dataset_id, normalized_term, evidence_type,
            original_region_tags, raw_tags, pos, targets
        ) VALUES (
            $1, 'local-probe', 'spelling', ARRAY['UK'],
            ARRAY['UK', 'alternative', 'slang'], 'noun', ARRAY['local-variant-probe']
        )
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();
    let (status, local_variant) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "local-probe"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{local_variant}");
    assert_eq!(
        local_variant["builtin_dictionary"]["suggested_forms"][0]["regional_variants"]["mode"],
        "common"
    );
}

#[sqlx::test]
async fn v3_detection_uses_generic_content_alternative_spelling_evidence(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    seed_dictionary_term(&pool, "formalise", "word", "common_unmarked").await;
    seed_dictionary_term(&pool, "formalize", "word", "common_unmarked").await;
    let dataset_id: i64 =
        sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status = 'active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        "UPDATE dictionary.terms SET pos = ARRAY['verb'] WHERE dataset_id = $1 AND normalized_term = ANY($2)",
    )
    .bind(dataset_id)
    .bind(["formalise", "formalize"])
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.content_imports (
            dataset_id, input_sha256, source_locator, source_version,
            record_count, parser_version
        ) VALUES (
            $1, repeat('b', 64), 'https://kaikki.org/test-source',
            'enwiktionary-content-test', 2, 'forms-sounds-v1'
        )
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.entry_contents (
            dataset_id, source_key, normalized_term, pos, senses,
            forms, sounds, source_locator
        ) VALUES
            ($1, 'kaikki:formalise:verb:test', 'formalise', 'verb', '[]'::jsonb,
             '[{"form":"formalize","tags":["alternative"]},
               {"form":"formalises","tags":["present","singular","third-person"]}]'::jsonb,
             '[{"form":"formalises","ipa":"/formalises/"}]'::jsonb,
             'https://kaikki.org/test-source'),
            ($1, 'kaikki:formalize:verb:test', 'formalize', 'verb', '[]'::jsonb,
             '[{"form":"formalise","tags":["alternative"]},
               {"form":"formalizes","tags":["present","singular","third-person"]}]'::jsonb,
             '[]'::jsonb, 'https://kaikki.org/test-source')
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();

    for input in ["formalise", "formalize"] {
        let (status, response) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/detections"),
            &bearer,
            None,
            Some(json!({
                "schema_version": 3,
                "language": "en",
                "kind": "word",
                "surface": input
            })),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{input} V3 检测失败：{response}");
        assert_eq!(
            response["builtin_dictionary"]["coverage"]["pronunciations"], "missing",
            "被删除派生词形上的唯一发音不得继续计入覆盖率"
        );
        assert_eq!(
            response["builtin_dictionary"]["suggested_forms"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "未可靠配对的派生词形不得跨英美侧复制"
        );
        assert_eq!(
            response["builtin_dictionary"]["provenance"]["forms"],
            json!({"name": "test", "version": "enwiktionary-content-test"})
        );
        assert_eq!(
            response["builtin_dictionary"]["suggested_forms"][0]["regional_variants"],
            json!({
                "mode": "uk_us",
                "uk": {"dialect": "uk", "spelling": "formalise", "pronunciations": []},
                "us": {"dialect": "us", "spelling": "formalize", "pronunciations": []}
            })
        );
    }

    seed_dictionary_term(&pool, "abbrev-probe", "word", "common_unmarked").await;
    seed_dictionary_term(&pool, "expanded-probe", "word", "common_unmarked").await;
    sqlx::query(
        r#"
        INSERT INTO dictionary.entry_contents (
            dataset_id, source_key, normalized_term, pos, senses,
            forms, sounds, source_locator
        ) VALUES
            ($1, 'kaikki:abbrev-probe:noun:test', 'abbrev-probe', 'noun', '[]'::jsonb,
             '[{"form":"expanded-probe","tags":["alternative","abbreviation"]}]'::jsonb,
             '[]'::jsonb, 'https://kaikki.org/test-source'),
            ($1, 'kaikki:expanded-probe:noun:test', 'expanded-probe', 'noun', '[]'::jsonb,
             '[{"form":"expanded-probes","tags":["plural"]}]'::jsonb,
             '[]'::jsonb, 'https://kaikki.org/test-source')
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();
    let (status, rejected_candidate) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "abbrev-probe"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rejected_candidate}");
    assert_eq!(
        rejected_candidate["builtin_dictionary"]["suggested_forms"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "被拒绝 candidate 的复数词形不得污染源词建议"
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
    assert_eq!(
        detection["builtin_dictionary"]["headwords"],
        json!({"mode": "unified", "common": "manual-common"}),
        "检测结果继续返回词典建议"
    );

    let (status, malformed) = call(
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
                "uk": "manual-common",
                "us": "manual-common"
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(malformed["code"], "invalid_request_body");

    let (status, empty) = call(
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
                "uk": "   ",
                "us": "manual-common",
                "source_dialect": "us"
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(empty["code"], "invalid_headword");

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
                "uk": "  manual-common  ",
                "us": " manual-common ",
                "source_dialect": "us"
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "统一改区分应成功：{created}");
    assert_eq!(created["word"]["headwords"]["mode"], "distinguish");
    assert_eq!(created["word"]["headwords"]["uk"], "manual-common");
    assert_eq!(created["word"]["headwords"]["us"], "manual-common");
    assert_eq!(
        created["word"]["detection_snapshot"]["headwords"],
        json!({"mode": "unified", "common": "manual-common"}),
        "持久化检测快照必须保留原始词典建议"
    );
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
        "管理员补出的英式侧必须标记为手工来源"
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
            "headwords": {"mode": "unified", "common": "manual-unified-edited"}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "区分改统一应成功：{created}");
    assert_eq!(created["word"]["headwords"]["mode"], "unified");
    assert_eq!(
        created["word"]["headwords"]["common"], "manual-unified-edited",
        "改为统一时也允许覆盖词典建议拼写"
    );
    assert_eq!(
        created["word"]["forms"]["pos"][0]["base_form"]["variants"][0]["origin"],
        "manual"
    );
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

    seed_dictionary_term(&pool, "source-edit-common", "word", "common_unmarked").await;
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "source-edit-common"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "主词编辑测试检测失败：{detection}");
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
                "uk": "source-edited-uk",
                "us": "source-edit-common",
                "source_dialect": "us"
            }
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "允许管理员修改非主词侧拼写：{created}"
    );
    assert_eq!(
        created["word"]["headwords"],
        json!({
            "mode": "distinguish",
            "uk": "source-edited-uk",
            "us": "source-edit-common",
            "source_dialect": "us"
        })
    );

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
                "uk": "single-uk-edited",
                "us": "single-uk",
                "source_dialect": "us"
            }
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "source_dialect 应表达管理员选择的主侧，不受检测命中方言限制：{created}"
    );
    assert_eq!(created["word"]["headwords"]["source_dialect"], "us");
}

#[sqlx::test]
async fn edited_matched_headwords_rebind_the_legacy_duplicate_fallback(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let duplicate = "edited-legacy-duplicate";
    let existing = create_incomplete_draft(&state, &pool, &bearer, duplicate).await;
    let existing_id = Uuid::parse_str(existing["word"]["id"].as_str().unwrap()).unwrap();
    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET is_deleted = TRUE, source_revision = 999999,
            event_offset = nextval('lexicon.surface_projection_event_offset_seq')
        WHERE entry_id = $1 AND source_kind = 'headword'
        "#,
    )
    .bind(existing_id)
    .execute(&pool)
    .await
    .unwrap();
    // B4 cutover 会移除旧唯一索引；之后 legacy key 只承担投影缺口兜底检查。
    sqlx::query("DROP INDEX lexicon.lexicon_entry_headword_keys_unique_idx")
        .execute(&pool)
        .await
        .unwrap();

    seed_dictionary_word(&pool, "edited-legacy-source").await;
    let (status, clear_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "edited-legacy-source"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "检测失败：{clear_detection}");
    assert_eq!(clear_detection["smart_dictionary"]["status"], "clear");

    let (status, rejected) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": clear_detection["detection_id"],
            "headwords": {"mode": "unified", "common": duplicate}
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "编辑后的最终词头命中 legacy-only 重复时必须拒绝：{rejected}"
    );
    assert_eq!(rejected["code"], "duplicate_word");

    let (status, duplicate_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": duplicate})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "重复词检测失败：{duplicate_detection}"
    );
    assert_eq!(
        duplicate_detection["smart_dictionary"]["status"],
        "duplicate"
    );

    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": duplicate_detection["detection_id"],
            "headwords": {"mode": "unified", "common": "edited-away-from-legacy-duplicate"}
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "检测建议是 legacy-only 重复但最终词头已改开时应允许创建：{created}"
    );
    assert_eq!(
        created["word"]["headwords"],
        json!({"mode": "unified", "common": "edited-away-from-legacy-duplicate"})
    );
}

#[sqlx::test]
async fn list_rows_order_headword_spellings_by_source_dialect(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 三个词条覆盖列表可能出现的全部基准侧形态：us 基准、uk 基准、无基准（unified）。
    for term in ["listorderus", "listorderuk", "listordercommon"] {
        seed_dictionary_word(&pool, term).await;
    }
    let cases = [
        (
            "listorderus",
            json!({
                "mode": "distinguish",
                "uk": "listorderusbre",
                "us": "listorderus",
                "source_dialect": "us",
            }),
        ),
        (
            "listorderuk",
            json!({
                "mode": "distinguish",
                "uk": "listorderuk",
                "us": "listorderukame",
                "source_dialect": "uk",
            }),
        ),
        (
            "listordercommon",
            json!({"mode": "unified", "common": "listordercommon"}),
        ),
    ];
    for (term, headwords) in &cases {
        let (status, detection) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/detections"),
            &bearer,
            None,
            Some(json!({"language": "en", "headword": term})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{term} 检测失败：{detection}");
        let (status, created) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/entries"),
            &bearer,
            Some(Uuid::now_v7()),
            Some(json!({
                "schema_version": 2,
                "detection_id": detection["detection_id"],
                "headwords": headwords,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{term} 建稿失败：{created}");
    }

    let row = |list: &Value| list["words"][0].clone();
    let fetch = async |query: &str| {
        let (status, list) = call(
            &state,
            Method::GET,
            &format!("{ROOT}/entries?q={query}"),
            &bearer,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{query} 列表查询失败：{list}");
        assert_eq!(list["words"].as_array().map(Vec::len), Some(1));
        list
    };

    let us_based = row(&fetch("listorderus").await);
    assert_eq!(
        us_based["headword"], "listorderus / listorderusbre",
        "美式基准词条的列表词汇列必须把基准侧排在前"
    );
    assert_eq!(us_based["dialects"], json!(["us", "uk"]));
    assert_eq!(us_based["source_dialect"], "us");
    // 结构化词头与 dialects 同序，让前端能按管理员方言偏好重排而不用切分 " / "。
    assert_eq!(
        us_based["headword_variants"],
        json!([
            {"dialect": "us", "headword": "listorderus"},
            {"dialect": "uk", "headword": "listorderusbre"},
        ])
    );

    let uk_based = row(&fetch("listorderuk").await);
    assert_eq!(
        uk_based["headword"], "listorderuk / listorderukame",
        "英式基准词条的列表词汇列必须把基准侧排在前"
    );
    assert_eq!(uk_based["dialects"], json!(["uk", "us"]));
    assert_eq!(uk_based["source_dialect"], "uk");
    assert_eq!(
        uk_based["headword_variants"],
        json!([
            {"dialect": "uk", "headword": "listorderuk"},
            {"dialect": "us", "headword": "listorderukame"},
        ])
    );

    let unified = row(&fetch("listordercommon").await);
    assert_eq!(unified["headword"], "listordercommon");
    assert_eq!(unified["dialects"], json!(["common"]));
    assert_eq!(
        unified["headword_variants"],
        json!([{"dialect": "common", "headword": "listordercommon"}]),
        "unified 词条也给结构化词头，只是单元素"
    );
    assert!(
        !unified.as_object().unwrap().contains_key("source_dialect"),
        "unified 词条没有基准侧，字段应整体省略：{unified}"
    );
}

#[sqlx::test]
async fn publishing_materializes_a_relation_word_that_has_no_entry_yet(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let source_headword = format!("matsource{}", admin_id.simple());
    let pending_headword = format!("matpending{}", admin_id.simple());
    let pending_gloss = "发布时预填的中文词义";
    let source = create_ready_draft(&state, &pool, &bearer, &source_headword).await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();

    // 库里没有这个词，管理员直接把它写成近义词。
    let dictionary_hit: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM lexicon.entry_headword_keys WHERE normalized_headword = $1)",
    )
    .bind(&pending_headword)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!dictionary_hit, "前置：待建词此刻不应存在");

    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "pending_target_headword": pending_headword,
        "pending_target_gloss": pending_gloss,
        "score": "88.00"
    }]);
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "草稿必须能存下待建关联词：{saved}");
    let saved_relation = &saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(saved_relation["pending_target_headword"], pending_headword);
    assert_eq!(saved_relation["pending_target_gloss"], pending_gloss);
    assert!(
        saved_relation["target_word_id"].is_null(),
        "草稿保存不得建条，target 必须还空着：{saved_relation}"
    );

    // 草稿保存不建条——这是「错字和弃稿不落成词条」的根据。
    let created_early: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM lexicon.entry_headword_keys WHERE normalized_headword = $1)",
    )
    .bind(&pending_headword)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!created_early, "草稿保存绝不能建出词条");

    let (status, published) = publish_ready(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "发布失败：{published}");

    // 发布把待建词物化成真实词条，并回填 target。
    let materialized: Option<Uuid> = sqlx::query_scalar(
        "SELECT entry_id FROM lexicon.entry_headword_keys WHERE normalized_headword = $1 LIMIT 1",
    )
    .bind(&pending_headword)
    .fetch_optional(&pool)
    .await
    .unwrap();
    let materialized = materialized.expect("发布后待建词必须已成词条");

    let published_relation = &published["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(
        published_relation["target_word_id"],
        materialized.to_string(),
        "发布出去的关联词必须已绑定：{published_relation}"
    );
    assert!(
        published_relation["pending_target_headword"].is_null(),
        "绑定后不得再留待建词面：{published_relation}"
    );
    assert!(
        published_relation["pending_target_gloss"].is_null(),
        "绑定后不得再留待建预定义词义：{published_relation}"
    );

    let (status, reloaded_source) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source_entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "发布后读取源词条失败：{reloaded_source}"
    );
    let reloaded_relation =
        &reloaded_source["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(
        reloaded_relation["target_word_id"],
        materialized.to_string(),
        "发布后的 canonical 草稿必须同步为已绑定关系：{reloaded_relation}"
    );
    assert!(
        reloaded_relation["pending_target_headword"].is_null(),
        "发布后的 canonical 草稿不得残留 pending headword：{reloaded_relation}"
    );
    assert!(
        reloaded_relation["pending_target_gloss"].is_null(),
        "发布后的 canonical 草稿不得残留 pending gloss：{reloaded_relation}"
    );

    let stored_editor_meanings: Value = sqlx::query_scalar(
        "SELECT meanings FROM lexicon.entry_editor_projection WHERE entry_id = $1",
    )
    .bind(source_entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let stored_editor_relation = &stored_editor_meanings["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(stored_editor_relation, reloaded_relation);

    let (stored_target, stored_pending_headword, stored_pending_gloss): (
        Option<Uuid>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT target_entry_id, pending_target_headword, pending_target_gloss FROM lexicon.relations WHERE entry_id = $1",
    )
    .bind(source_entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored_target, Some(materialized));
    assert_eq!(stored_pending_headword, None);
    assert_eq!(stored_pending_gloss, None);

    let (status, resaved_source) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": reloaded_source["word"]["revision"],
            "intent": "complete",
            "content": reloaded_source["word"]["meanings"].clone(),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "发布后 canonical 草稿应可再次保存：{resaved_source}"
    );
    let (status, republished_source) = publish_ready(&state, &bearer, &resaved_source).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "发布后再次保存并重复发布应保持可用：{republished_source}"
    );

    let (status, materialized_word) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{materialized}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "读取物化词条失败：{materialized_word}"
    );
    assert_eq!(
        materialized_word["word"]["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"],
        pending_gloss,
        "预定义词义必须写入新建词条的默认中文释义：{materialized_word}"
    );

    // 占位是普通草稿，带一个词性和一个可被指向的义项。
    let (kind, status_row): (String, bool) = sqlx::query_as(
        "SELECT kind, current_publication_id IS NOT NULL FROM lexicon.entries WHERE id = $1",
    )
    .bind(materialized)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kind, "word");
    assert!(!status_row, "占位应当是草稿");
    let sense_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.senses WHERE entry_id = $1")
            .bind(materialized)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(sense_count, 1, "关联词必须有义项可指");

    // 审计如实记录这个词条为什么存在。
    let audit: Option<Uuid> = sqlx::query_scalar(
        "SELECT resource_id FROM audit.admin_actions
         WHERE action = 'lexicon.entry.materialize_relation_target' AND resource_id = $1",
    )
    .bind(materialized)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(audit, Some(materialized), "物化必须留下独立审计动作");

    // 闭环最后一步：再检测这个词，它已经命中词典。
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": pending_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "检测失败：{detection}");
    assert_eq!(
        detection["smart_dictionary"]["status"], "warning",
        "物化出来的占位必须能在下次录入时被检测到：{detection}"
    );
}

#[sqlx::test]
async fn saving_rejects_conflicting_glosses_for_the_same_pending_relation_target(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let source = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("glossconflictsrc{}", admin_id.simple()),
    )
    .await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let pending_headword = format!("glossconflicttarget{}", admin_id.simple());
    let first_relation_id = Uuid::now_v7();
    let second_relation_id = Uuid::now_v7();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([
        {
            "id": first_relation_id,
            "relation": "synonym",
            "pending_target_headword": pending_headword,
            "pending_target_gloss": "第一个预定义词义",
            "score": "80.00"
        },
        {
            "id": second_relation_id,
            "relation": "antonym",
            "pending_target_headword": pending_headword,
            "pending_target_gloss": "另一个预定义词义",
            "score": "60.00"
        }
    ]);

    let (status, blocked) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "应拒绝冲突词义：{blocked}"
    );
    let conflict = blocked["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "relation_pending_gloss_conflict")
        .unwrap_or_else(|| panic!("应返回稳定的预定义词义冲突错误：{blocked}"));
    assert_eq!(conflict["field"], "pending_target_gloss");
    assert!(
        conflict["node_id"] == first_relation_id.to_string()
            || conflict["node_id"] == second_relation_id.to_string(),
        "错误必须锚定冲突的关联词行：{conflict}"
    );
}

#[sqlx::test]
async fn saving_does_not_discard_a_pending_gloss_when_the_target_already_exists(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_headword = format!("existinggloss{}", admin_id.simple());
    let target = create_ready_draft(&state, &pool, &bearer, &target_headword).await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    let original_gloss =
        target["word"]["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"]
            .clone();
    let source = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("existingglosssrc{}", admin_id.simple()),
    )
    .await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let relation_id = Uuid::now_v7();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id,
        "relation": "synonym",
        "pending_target_headword": target_headword,
        "pending_target_gloss": "不得覆盖已有词条",
        "score": "80.00"
    }]);

    let (status, blocked) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "应拒绝静默丢弃：{blocked}"
    );
    let issue = blocked["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "relation_pending_gloss_target_exists")
        .unwrap_or_else(|| panic!("应返回已有目标错误：{blocked}"));
    assert_eq!(issue["node_id"], relation_id.to_string());
    assert_eq!(issue["field"], "pending_target_gloss");

    let (status, reread) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{target_entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "读取已有目标失败：{reread}");
    assert_eq!(
        reread["word"]["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"],
        original_gloss,
        "已有目标内容不得被预定义词义覆盖"
    );
}

#[sqlx::test]
async fn saving_a_draft_binds_a_pending_relation_once_the_word_exists(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_headword = format!("bindlater{}", admin_id.simple());
    let source = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("bindsrc{}", admin_id.simple()),
    )
    .await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();

    let relation_id = Uuid::now_v7();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id,
        "relation": "synonym",
        "pending_target_headword": target_headword,
        "score": "75.00"
    }]);
    let save = |content: Value, base_revision: Value| {
        let state = state.clone();
        let bearer = bearer.clone();
        async move {
            call(
                &state,
                Method::PUT,
                &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
                &bearer,
                None,
                Some(json!({
                    "base_revision": base_revision,
                    "intent": "complete",
                    "content": content,
                })),
            )
            .await
        }
    };

    // 目标还不存在：存下来仍是待物化形态。
    let (status, saved) = save(meanings.clone(), source["word"]["revision"].clone()).await;
    assert_eq!(status, StatusCode::OK, "保存失败：{saved}");
    let stored = &saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert!(
        stored["target_word_id"].is_null(),
        "目标不存在时必须留在待物化形态：{stored}"
    );

    // 目标词条被单独建出来之后，下一次保存草稿就顺带绑上——只绑不建。
    let target = create_ready_draft(&state, &pool, &bearer, &target_headword).await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    let entry_count_before: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();

    let (status, rebound) = save(
        saved["word"]["meanings"].clone(),
        saved["word"]["revision"].clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "再次保存失败：{rebound}");
    let bound = &rebound["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(
        bound["target_word_id"],
        target_entry_id.to_string(),
        "同名词条已存在时，保存草稿应当顺带绑定：{bound}"
    );
    assert!(
        bound["pending_target_headword"].is_null(),
        "绑定后不得再留待建词面：{bound}"
    );

    let entry_count_after: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        entry_count_before, entry_count_after,
        "草稿保存只绑不建，词条总数不得变化"
    );
}

#[sqlx::test]
async fn materialization_binds_to_an_existing_entry_instead_of_creating_a_twin(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let shared_target = format!("mattwin{}", admin_id.simple());
    let first = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("matone{}", admin_id.simple()),
    )
    .await;
    let second = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("mattwo{}", admin_id.simple()),
    )
    .await;

    // 两个词条各自把同一个还不存在的词写成关联词，先后发布。
    let mut materialized_ids = Vec::new();
    for source in [&first, &second] {
        let entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
        let mut meanings = source["word"]["meanings"].clone();
        meanings["pos"][0]["senses"][0]["relations"] = json!([{
            "id": Uuid::now_v7(),
            "relation": "synonym",
            "pending_target_headword": shared_target,
            "score": "70.00"
        }]);
        let (status, saved) = call(
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
        assert_eq!(status, StatusCode::OK, "保存失败：{saved}");
        let (status, published) = publish_ready(&state, &bearer, &saved).await;
        assert_eq!(status, StatusCode::CREATED, "发布失败：{published}");
        materialized_ids.push(
            published["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_word_id"]
                .as_str()
                .expect("发布出去的关联词必须已绑定")
                .to_owned(),
        );
    }

    assert_eq!(
        materialized_ids[0], materialized_ids[1],
        "同名关联词必须绑到同一个词条，不能各建一条"
    );
    let entry_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.entry_headword_keys WHERE normalized_headword = $1",
    )
    .bind(&shared_target)
    .fetch_one(&pool)
    .await
    .unwrap();
    // 每个词条按 uk/us 两个 dialect_scope 各占一行，所以一个词条是 2。
    assert_eq!(entry_count, 2, "只应存在一个同名词条");
}

#[sqlx::test]
async fn materialization_refuses_a_target_entry_that_has_no_sense(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 未收录词建出来的草稿没有词典建议，也就没有词性和词义节点。
    let bare_headword = format!("matbare{}", admin_id.simple());
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": bare_headword})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "检测失败：{detection}");
    let (status, bare) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {"mode": "unified", "common": bare_headword},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "建稿失败：{bare}");
    let sense_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.senses WHERE entry_id = $1")
            .bind(Uuid::parse_str(bare["word"]["id"].as_str().unwrap()).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(sense_count, 0, "前置：目标词条此刻没有词义");

    let source = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("matref{}", admin_id.simple()),
    )
    .await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    // 第二个待建词是可以建出来的，用它见证「事务一起回滚」。
    let rollback_witness = format!("matrollback{}", admin_id.simple());
    let relation_id = Uuid::now_v7();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([
        {
            "id": Uuid::now_v7(),
            "relation": "derivative",
            "pending_target_headword": rollback_witness,
            "score": "50.00"
        },
        {
            "id": relation_id,
            "relation": "synonym",
            "pending_target_headword": bare_headword,
            "score": "60.00"
        }
    ]);
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "草稿保存不该受目标状态影响：{saved}"
    );

    // 同名词条已存在但没有词义可指——报可操作的错误，不去改别人的草稿。
    let (status, blocked) = publish_ready(&state, &bearer, &saved).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "应被拦下：{blocked}"
    );
    let issue = blocked["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "relation_target_has_no_sense")
        .unwrap_or_else(|| panic!("错误必须说清是目标词条缺词义：{blocked}"));
    // 锚回具体那一条关联词，否则前端只能指向词条本身，管理员不知道该改哪一行。
    assert_eq!(issue["node_id"], relation_id.to_string());
    assert_eq!(issue["field"], "pending_target_headword");

    // 同一次发布里另一个待建词已经建成功了，但整笔事务必须一起回滚——
    // 「发布失败不留没人引用的占位」是这个设计不产生脏数据的前提。
    let rollback_probe: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.entry_headword_keys WHERE normalized_headword = $1",
    )
    .bind(&rollback_witness)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rollback_probe, 0, "发布失败时先建出来的占位必须跟着回滚");
}

/// 关联词搜索只搜「已发布且未归档」的词条，所以同名词条一旦归档，管理员在下拉里
/// 看不到它，只会把这个词当库外新词写成待建关联词。此时绑上去就是死结：草稿存得下、
/// 发布必被拒，而待建词面已被清空，重填同一个词还会再绑上来。必须在绑定这一步拦住。
#[sqlx::test]
async fn saving_a_draft_refuses_to_bind_a_pending_relation_onto_an_archived_twin(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_headword = format!("arctarget{}", admin_id.simple());
    let target = create_ready_draft(&state, &pool, &bearer, &target_headword).await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": target["word"]["revision"],
            "base_lifecycle_revision": target["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "归档目标词条失败：{archived}");

    let source = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("arcsource{}", admin_id.simple()),
    )
    .await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let relation_id = Uuid::now_v7();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id,
        "relation": "synonym",
        "pending_target_headword": target_headword,
        "score": "70.00"
    }]);
    let save = |content: Value, base_revision: Value| {
        let state = state.clone();
        let bearer = bearer.clone();
        async move {
            call(
                &state,
                Method::PUT,
                &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
                &bearer,
                None,
                Some(json!({
                    "base_revision": base_revision,
                    "intent": "complete",
                    "content": content,
                })),
            )
            .await
        }
    };

    let (status, blocked) = save(meanings.clone(), source["word"]["revision"].clone()).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "同名词条已归档时草稿保存就该被拦下：{blocked}"
    );
    let issue = blocked["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "relation_target_archived")
        .unwrap_or_else(|| panic!("错误必须说清是同名词条已归档：{blocked}"));
    // 锚回具体那一条关联词，并指向管理员实际填过的字段——他没填过 target_sense_id。
    assert_eq!(issue["node_id"], relation_id.to_string());
    assert_eq!(issue["field"], "pending_target_headword");

    // 被拒的保存必须整笔回滚——revision 不许往前走，否则前端手里的 base_revision
    // 会平白失效，管理员改完词面再存就撞 revision 冲突，等于换了个方式卡死。
    let (status, reread) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source_entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "重读词条失败：{reread}");
    assert_eq!(
        reread["word"]["revision"], source["word"]["revision"],
        "被拒的保存不得推进 revision：{reread}"
    );

    // 出路必须真的存在：恢复那条词条之后，同一份草稿就能存下并绑上去。
    let (status, restored) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_entry_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": archived["word"]["revision"],
            "base_lifecycle_revision": archived["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "恢复目标词条失败：{restored}");

    let (status, saved) = save(meanings, source["word"]["revision"].clone()).await;
    assert_eq!(status, StatusCode::OK, "恢复之后应能存下：{saved}");
    let bound = &saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(
        bound["target_word_id"],
        target_entry_id.to_string(),
        "恢复之后应当绑上原来那条词条：{bound}"
    );
}

/// 待建关联词是先存下、后发布的，目标可能在这中间才被建出来并归档。发布时的物化
/// 同样不能绑上去——归档词条占着词头唯一键，绕过它另建同名新条会撞唯一索引。
#[sqlx::test]
async fn publishing_refuses_to_materialize_a_pending_relation_onto_an_archived_twin(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_headword = format!("arclate{}", admin_id.simple());
    let source = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("arclatesrc{}", admin_id.simple()),
    )
    .await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let relation_id = Uuid::now_v7();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id,
        "relation": "synonym",
        "pending_target_headword": target_headword,
        "score": "65.00"
    }]);

    // 存下时库里还没有这个词，草稿留在待物化形态。
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "保存失败：{saved}");
    assert!(
        saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_word_id"]
            .is_null(),
        "前置：目标不存在时应留在待物化形态：{saved}"
    );

    // 之后别人把这个词建了出来，又把它归档了。
    let target = create_ready_draft(&state, &pool, &bearer, &target_headword).await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": target["word"]["revision"],
            "base_lifecycle_revision": target["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "归档目标词条失败：{archived}");

    let (status, blocked) = publish_ready(&state, &bearer, &saved).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "应被拦下：{blocked}"
    );
    let issue = blocked["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "relation_target_archived")
        .unwrap_or_else(|| panic!("错误必须说清是同名词条已归档：{blocked}"));
    assert_eq!(issue["node_id"], relation_id.to_string());
    assert_eq!(issue["field"], "pending_target_headword");

    // 既没绑上归档词条，也没另建一条同名的——那会撞词头唯一索引。
    let twin_count: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT entry_id) FROM lexicon.entry_headword_keys
         WHERE normalized_headword = $1",
    )
    .bind(&target_headword)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(twin_count, 1, "不得为同一个词面再建一条词条");
    let pending: Option<String> =
        sqlx::query_scalar("SELECT pending_target_headword FROM lexicon.relations WHERE id = $1")
            .bind(relation_id)
            .fetch_optional(&pool)
            .await
            .unwrap()
            .flatten();
    assert_eq!(
        pending.as_deref(),
        Some(target_headword.as_str()),
        "发布失败后关联词必须仍是待物化形态，词面还在管理员手上"
    );
}

/// 词形步骤保存也会顺带绑定待物化关联词（`save_forms` 与 `save_meanings` 各有一个
/// `BindExisting` 调用点），所以同名词条被第三方归档之后，管理员**在词形步骤也存不下**。
///
/// 这是有意的：放行就等于让词形保存把关联词绑成死结。但代价是错误落在一个他当时
/// 看不见的地方——`reference_issue` 把 `step` 硬编码成 `meanings`，`node_id` 指向词义
/// 步骤的关联词节点。这里把这个形态钉住，将来要调整必须是有意为之。
#[sqlx::test]
async fn saving_the_forms_step_is_blocked_by_an_archived_relation_twin_too(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_headword = format!("arcforms{}", admin_id.simple());
    let source = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("arcformssrc{}", admin_id.simple()),
    )
    .await;
    let source_entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let relation_id = Uuid::now_v7();
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id,
        "relation": "synonym",
        "pending_target_headword": target_headword,
        "score": "55.00"
    }]);

    // 存下时库里还没有这个词，关联词留在待物化形态。
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": source["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "保存失败：{saved}");

    // 之后别人把这个词建了出来，又把它归档了。
    let target = create_ready_draft(&state, &pool, &bearer, &target_headword).await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": target["word"]["revision"],
            "base_lifecycle_revision": target["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "归档目标词条失败：{archived}");

    // 管理员回来改词形——内容原样重存，跟关联词毫无关系。
    let (status, blocked) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": saved["word"]["revision"],
            "intent": "complete",
            "content": saved["word"]["forms"],
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "词形步骤同样绑定待物化关联词，归档目标必须一并拦下：{blocked}"
    );
    let issue = blocked["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "relation_target_archived")
        .unwrap_or_else(|| panic!("错误必须说清是同名词条已归档：{blocked}"));
    // 已知的错位：人在词形步骤，issue 却锚在词义步骤的关联词节点上。前端得据此
    // 把管理员引到词义步骤，不能就地渲染。
    assert_eq!(issue["step"], "meanings");
    assert_eq!(issue["node_id"], relation_id.to_string());
    assert_eq!(issue["field"], "pending_target_headword");

    // 词面仍在草稿里，管理员去词义步骤就能改。
    let pending: Option<String> =
        sqlx::query_scalar("SELECT pending_target_headword FROM lexicon.relations WHERE id = $1")
            .bind(relation_id)
            .fetch_optional(&pool)
            .await
            .unwrap()
            .flatten();
    assert_eq!(pending.as_deref(), Some(target_headword.as_str()));
}

#[sqlx::test]
async fn relation_target_headwords_follow_the_source_dialect(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 三个关联目标覆盖全部词头形态：us 基准、uk 基准、无基准（unified）。
    let targets = [
        (
            "reltargetus",
            Some(json!({
                "mode": "distinguish",
                "uk": "reltargetusbre",
                "us": "reltargetus",
                "source_dialect": "us",
            })),
            "reltargetus / reltargetusbre",
        ),
        (
            "reltargetuk",
            Some(json!({
                "mode": "distinguish",
                "uk": "reltargetuk",
                "us": "reltargetukame",
                "source_dialect": "uk",
            })),
            "reltargetuk / reltargetukame",
        ),
        ("reltargetcommon", None, "reltargetcommon"),
    ];

    let mut relations = Vec::new();
    let mut expected = Vec::new();
    for (term, headwords, expected_headword) in &targets {
        let ready =
            create_ready_draft_with_headwords(&state, &pool, &bearer, term, headwords.clone())
                .await;
        let (status, published) = publish_ready(&state, &bearer, &ready).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{term} 目标词发布失败：{published}"
        );
        relations.push(json!({
            "id": Uuid::now_v7(),
            "relation": "synonym",
            "target_word_id": ready["word"]["id"],
            "target_sense_id": ready["word"]["meanings"]["pos"][0]["senses"][0]["id"],
            "target_headword": "客户端伪造词头",
            "target_gloss": "客户端伪造释义",
            "score": "88.50"
        }));
        expected.push((*expected_headword, format!("{term} 的释义")));
    }

    let source_ready = create_ready_draft(&state, &pool, &bearer, "relsource").await;
    let source_entry_id =
        Uuid::parse_str(source_ready["word"]["id"].as_str().unwrap()).expect("来源词 id 应是 uuid");
    let mut source_meanings = source_ready["word"]["meanings"].clone();
    source_meanings["pos"][0]["senses"][0]["relations"] = json!(relations);
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

    // Canonicalize：服务端覆盖写入的词头必须是「检测基准侧优先」。
    let saved_relations = source_saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"]
        .as_array()
        .expect("关联应原样返回");
    assert_eq!(saved_relations.len(), expected.len());
    for (saved, (expected_headword, expected_gloss)) in saved_relations.iter().zip(&expected) {
        assert_eq!(
            saved["target_headword"], *expected_headword,
            "关联目标词头必须把基准侧排在前：{saved}"
        );
        assert_eq!(saved["target_gloss"], *expected_gloss);
    }

    // Verify：同一份草稿必须能直接发布，否则「保存能过、发布必失败」。
    let (status, source_published) = publish_ready(&state, &bearer, &source_saved).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "保存与发布必须用同一套词头顺序，不能被 relation_target_stale 拦下：{source_published}"
    );

    let snapshots: Vec<String> = sqlx::query_scalar(
        "SELECT target_headword_snapshot FROM lexicon.relations WHERE entry_id = $1 ORDER BY sort_order",
    )
    .bind(source_entry_id)
    .fetch_all(&pool)
    .await
    .expect("应能读取关联快照");
    assert_eq!(
        snapshots,
        expected
            .iter()
            .map(|(headword, _)| (*headword).to_owned())
            .collect::<Vec<_>>(),
        "落库的词头快照必须与保存/发布时一致"
    );
}

#[sqlx::test]
async fn related_search_orders_headword_spellings_like_the_list(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let cases = [
        (
            "relorderus",
            Some(json!({
                "mode": "distinguish",
                "uk": "relorderusbre",
                "us": "relorderus",
                "source_dialect": "us",
            })),
            "relorderus / relorderusbre",
            json!(["us", "uk"]),
        ),
        (
            "relorderuk",
            Some(json!({
                "mode": "distinguish",
                "uk": "relorderuk",
                "us": "relorderukame",
                "source_dialect": "uk",
            })),
            "relorderuk / relorderukame",
            json!(["uk", "us"]),
        ),
        ("relordercommon", None, "relordercommon", json!(["common"])),
    ];

    for (term, headwords, expected_headword, expected_dialects) in &cases {
        let ready =
            create_ready_draft_with_headwords(&state, &pool, &bearer, term, headwords.clone())
                .await;
        let (status, published) = publish_ready(&state, &bearer, &ready).await;
        assert_eq!(status, StatusCode::CREATED, "{term} 发布失败：{published}");

        let (status, search) = call(
            &state,
            Method::GET,
            &format!("{ROOT}/entries/related-search?q={term}&kind=word&page_size=20"),
            &bearer,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{term} 关联词搜索失败：{search}");
        assert_eq!(
            search["results"].as_array().map(Vec::len),
            Some(1),
            "{term} 应只命中自己：{search}"
        );
        let result = &search["results"][0];
        assert_eq!(
            result["headword"], *expected_headword,
            "关联词搜索必须把基准侧排在前：{result}"
        );
        assert_eq!(
            result["dialects"], *expected_dialects,
            "dialects 必须与 headword 同序：{result}"
        );
        // 结构化词头是同一份数据的另一种形状：方言顺序与 dialects 逐位相同，
        // 拼写按序拼起来必须逐字符等于 headword，前端因此不必切分 " / "。
        let variants = result["headword_variants"]
            .as_array()
            .expect("关联词搜索结果应带结构化词头");
        assert_eq!(
            variants
                .iter()
                .map(|variant| variant["dialect"].clone())
                .collect::<Vec<_>>(),
            *expected_dialects.as_array().expect("期望方言应为数组"),
            "headword_variants 必须与 dialects 同序：{result}"
        );
        assert_eq!(
            variants
                .iter()
                .map(|variant| variant["headword"].as_str().expect("拼写应为字符串"))
                .collect::<Vec<_>>()
                .join(" / "),
            *expected_headword,
            "headword_variants 按序拼接必须等于 headword：{result}"
        );

        let (status, list) = call(
            &state,
            Method::GET,
            &format!("{ROOT}/entries?q={term}"),
            &bearer,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{term} 列表查询失败：{list}");
        assert_eq!(list["words"].as_array().map(Vec::len), Some(1));
        let row = &list["words"][0];
        assert_eq!(
            row["headword"], result["headword"],
            "列表与关联词搜索的并列拼写必须一致：{row} / {result}"
        );
        assert_eq!(
            row["dialects"], result["dialects"],
            "列表与关联词搜索的方言顺序必须一致：{row} / {result}"
        );
        assert_eq!(
            row["headword_variants"], result["headword_variants"],
            "列表与关联词搜索的结构化词头必须一致：{row} / {result}"
        );
    }

    // 排序键与展示串必须是同一个字符串。relsortcolos 正好落在两种拼法之间：
    // 按展示串 "relsortcolor / ..." 排它在后，按 uk-first 的 "relsortcolour / ..." 排它在前。
    for (term, headwords) in [
        (
            "relsortcolor",
            Some(json!({
                "mode": "distinguish",
                "uk": "relsortcolour",
                "us": "relsortcolor",
                "source_dialect": "us",
            })),
        ),
        ("relsortcolos", None),
    ] {
        let ready =
            create_ready_draft_with_headwords(&state, &pool, &bearer, term, headwords).await;
        let (status, published) = publish_ready(&state, &bearer, &ready).await;
        assert_eq!(status, StatusCode::CREATED, "{term} 发布失败：{published}");
    }
    let (status, sorted) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=relsortcolo&kind=word&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "排序查询失败：{sorted}");
    assert_eq!(
        sorted["results"]
            .as_array()
            .expect("结果应是数组")
            .iter()
            .map(|result| result["headword"].clone())
            .collect::<Vec<_>>(),
        vec![json!("relsortcolor / relsortcolour"), json!("relsortcolos")],
        "关联词搜索必须按展示出来的词头排序：{sorted}"
    );
}

/// 批内互相引用的词条一起删除时，结果不得取决于 id 排序。
/// 回归的是这样一个缺陷：批量按 target.id 排序后逐条「校验+删除」，
/// 先删掉的那条会级联清除它的出站引用，从而改变后一条的入站引用检查结果——
/// 于是同样的引用关系，A.id < B.id 时整批成功、B.id < A.id 时整批 409。
#[sqlx::test]
async fn delete_batch_ignores_references_between_members_regardless_of_id_order(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 两种创建顺序各跑一遍：唯一差别是引用方与被引用方谁的 id 更小。
    for (slug, label, referrer_first) in [
        ("srcfirst", "引用方 id 更小", true),
        ("dstfirst", "被引用方 id 更小", false),
    ] {
        let (referrer, referenced) = if referrer_first {
            let referrer =
                create_ready_draft(&state, &pool, &bearer, &format!("member-src-{slug}")).await;
            let referenced =
                create_ready_draft(&state, &pool, &bearer, &format!("member-dst-{slug}")).await;
            (referrer, referenced)
        } else {
            let referenced =
                create_ready_draft(&state, &pool, &bearer, &format!("member-dst-{slug}")).await;
            let referrer =
                create_ready_draft(&state, &pool, &bearer, &format!("member-src-{slug}")).await;
            (referrer, referenced)
        };
        let referrer_id = Uuid::parse_str(referrer["word"]["id"].as_str().unwrap()).unwrap();
        let referenced_id = Uuid::parse_str(referenced["word"]["id"].as_str().unwrap()).unwrap();
        let referrer_sense_id = Uuid::parse_str(
            referrer["word"]["meanings"]["pos"][0]["senses"][0]["id"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let referenced_sense_id = Uuid::parse_str(
            referenced["word"]["meanings"]["pos"][0]["senses"][0]["id"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            referrer_first,
            referrer_id < referenced_id,
            "{label}：构造出的 id 顺序与预期不符"
        );

        // referrer --近义词--> referenced
        let relation_id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO lexicon.nodes (
                id, entry_id, node_type, parent_node_id, node_role, stable_slot
            ) VALUES ($1, $2, 'relation', $3, 'meanings.relation', false)
            "#,
        )
        .bind(relation_id)
        .bind(referrer_id)
        .bind(referrer_sense_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO lexicon.relations (
                id, entry_id, source_sense_id, relation_type,
                target_entry_id, target_sense_id, score,
                target_headword_snapshot, target_gloss_snapshot, sort_order
            ) VALUES ($1, $2, $3, 'synonym', $4, $5, 100, 'member', '', 0)
            "#,
        )
        .bind(relation_id)
        .bind(referrer_id)
        .bind(referrer_sense_id)
        .bind(referenced_id)
        .bind(referenced_sense_id)
        .execute(&pool)
        .await
        .unwrap();

        let mut entries = Vec::new();
        for word in [&referrer, &referenced] {
            let id = word["word"]["id"].as_str().unwrap().to_owned();
            let (status, archived) = call(
                &state,
                Method::POST,
                &format!("{ROOT}/entries/{id}/archive"),
                &bearer,
                Some(Uuid::now_v7()),
                Some(json!({
                    "base_revision": word["word"]["revision"],
                    "base_lifecycle_revision": word["word"]["lifecycle_revision"]
                })),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{label} 归档失败：{archived}");
            entries.push(json!({
                "id": id,
                "base_revision": archived["word"]["revision"],
                "base_lifecycle_revision": archived["word"]["lifecycle_revision"]
            }));
        }

        let (status, response) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/entries/delete-batch"),
            &bearer,
            Some(Uuid::now_v7()),
            Some(json!({ "entries": entries })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{label}：批内互相引用不应阻塞整批删除——两条都要删掉，谁先谁后不该改变结果：{response}"
        );
        assert_eq!(response["affected"], 2, "{label}");
    }
}

#[sqlx::test]
async fn entry_reference_summary_dedupes_sources_and_matches_the_delete_gate(pool: PgPool) {
    // 本测试守的是整个功能最关键的不变量：**计数为 0 的词条一定能删，
    // 计数大于 0 的一定删不掉**。计数与删除拦截若各写一套口径，就会出现
    // 「显示 0 引用却删不掉」——比不显示引用数更糟。
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let referenced = create_ready_draft(&state, &pool, &bearer, "refcount-target").await;
    let referring = create_ready_draft(&state, &pool, &bearer, "refcount-source").await;
    let referenced_id = Uuid::parse_str(referenced["word"]["id"].as_str().unwrap()).unwrap();
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

    let summary_of = |list: &Value| -> Value {
        list["words"]
            .as_array()
            .unwrap()
            .iter()
            .find(|word| word["id"] == referenced_id.to_string())
            .expect("列表应包含目标词条")["reference_summary"]
            .clone()
    };

    // 无人引用：total = 0。
    let (status, list) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?q=refcount-target"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let summary = summary_of(&list);
    assert_eq!(summary["total"], 0, "无人引用时应为 0：{summary}");
    assert_eq!(summary["previews"].as_array().unwrap().len(), 0);
    assert_eq!(summary["truncated"], false);

    // 挂上一条关联词引用（草稿态引用同样要计入）。
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
        ) VALUES ($1, $2, $3, 'synonym', $4, $5, 100, 'refcount-target', '', 0)
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

    let (_, list) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?q=refcount-target"),
        &bearer,
        None,
        None,
    )
    .await;
    let summary = summary_of(&list);
    assert_eq!(summary["total"], 1, "被一个词条引用：{summary}");
    let preview = &summary["previews"][0];
    assert_eq!(preview["source_word_id"], referring_id.to_string());
    assert_eq!(preview["source_headword"], "refcount-source");
    assert_eq!(preview["source_kind"], "relation");
    assert_eq!(preview["source_status"], "draft", "草稿引用也要计入");
    assert_eq!(summary["truncated"], false);

    // 同一个引用方再通过另一条路径引用：去重后仍是 1 个依赖方。
    sqlx::query(
        r#"
        INSERT INTO lexicon.sentence_associations (
            id, entry_id, sentence_id, source_dialect,
            range_start, range_end, surface, origin, state,
            target_entry_id, target_sense_id,
            target_headword_snapshot, target_gloss_snapshot, resolved_pos
        ) VALUES ($1, $2, $3, 'common', 0, 5, 'refcnt', 'manual', 'linked',
                  $4, $5, 'refcount-target', '', 'noun')
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(referring_id)
    .bind(referring_sense_id)
    .bind(referenced_id)
    .bind(referenced_sense_id)
    .execute(&pool)
    .await
    .unwrap();

    let (_, list) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?q=refcount-target"),
        &bearer,
        None,
        None,
    )
    .await;
    let summary = summary_of(&list);
    assert_eq!(
        summary["total"], 1,
        "同一引用方多路引用只算一个依赖方：{summary}"
    );
    assert_eq!(summary["previews"].as_array().unwrap().len(), 1);

    // 不变量：total > 0 时删除必须被拒。
    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{referenced_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": referenced["word"]["revision"],
            "base_lifecycle_revision": referenced["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "计数大于 0 的词条必须删不掉：{response}"
    );

    // 反向不变量：无人引用的词条 total = 0 且确实能删。
    let (_, list) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?q=refcount-source"),
        &bearer,
        None,
        None,
    )
    .await;
    let free = list["words"]
        .as_array()
        .unwrap()
        .iter()
        .find(|word| word["id"] == referring_id.to_string())
        .expect("列表应包含引用方词条")
        .clone();
    assert_eq!(free["reference_summary"]["total"], 0, "引用方自己无人引用");
    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{referring_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": free["revision"],
            "base_lifecycle_revision": free["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "计数为 0 的词条必须能删：{response}"
    );
}

#[sqlx::test]
async fn deleting_an_entry_referenced_by_a_sentence_association_conflicts(pool: PgPool) {
    // sentence_associations 的 target_shape_check 允许 state='linked' 且不带
    // target_publication_id——即例句关联可以指向一个尚未发布的词条。这条路径
    // 不在删除时的入站引用检查里就会撞上 DB 外键，冒出 500 而不是 409。
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let referenced = create_ready_draft(&state, &pool, &bearer, "assoc-target").await;
    let referring = create_ready_draft(&state, &pool, &bearer, "assoc-source").await;
    let referenced_id = Uuid::parse_str(referenced["word"]["id"].as_str().unwrap()).unwrap();
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

    // 先归档目标，模拟「垃圾桶里的词条被别处例句引用」。
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{referenced_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": referenced["word"]["revision"],
            "base_lifecycle_revision": referenced["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "归档失败：{archived}");
    let archived_revision = archived["word"]["revision"].as_i64().unwrap();
    let archived_lifecycle_revision = archived["word"]["lifecycle_revision"].as_i64().unwrap();

    sqlx::query(
        r#"
        INSERT INTO lexicon.sentence_associations (
            id, entry_id, sentence_id, source_dialect,
            range_start, range_end, surface, origin, state,
            target_entry_id, target_sense_id,
            target_headword_snapshot, target_gloss_snapshot, resolved_pos
        ) VALUES ($1, $2, $3, 'common', 0, 5, 'assoc', 'manual', 'linked',
                  $4, $5, 'assoc-target', '', 'noun')
        "#,
    )
    .bind(Uuid::now_v7())
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
            "base_revision": archived_revision,
            "base_lifecycle_revision": archived_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "被例句关联引用的词条必须以 409 拒绝，而不是撞外键冒 500：{response}"
    );
    assert_eq!(response["code"], "entry_not_deletable");
}

#[sqlx::test]
async fn deleting_an_entry_is_restricted_to_its_creator_unless_super_admin(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let owner_id = seed_admin(&pool).await;
    let owner_bearer = token(&state, owner_id);
    let other_id = seed_admin(&pool).await;
    let other_bearer = token(&state, other_id);
    let super_id = seed_admin_with_role(&pool, AdminRole::SuperAdmin).await;
    let super_bearer = token(&state, super_id);

    // 归属：他人创建的词条，普通管理员删不了。
    let owned = create_ready_draft(&state, &pool, &owner_bearer, "owned-by-creator").await;
    let owned_id = owned["word"]["id"].as_str().unwrap();
    let owned_revision = owned["word"]["revision"].as_i64().unwrap();
    let owned_lifecycle_revision = owned["word"]["lifecycle_revision"].as_i64().unwrap();
    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{owned_id}"),
        &other_bearer,
        None,
        Some(json!({
            "base_revision": owned_revision,
            "base_lifecycle_revision": owned_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "普通管理员不得删除他人创建的词条：{response}"
    );
    assert_eq!(response["code"], "entry_delete_forbidden");

    // 归属放行：创建者本人可以删。
    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{owned_id}"),
        &owner_bearer,
        None,
        Some(json!({
            "base_revision": owned_revision,
            "base_lifecycle_revision": owned_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "创建者本人应能删除自己的词条：{response}"
    );

    // 超管不受创建人限制。
    let foreign = create_ready_draft(&state, &pool, &owner_bearer, "owned-but-super-deletes").await;
    let foreign_id = foreign["word"]["id"].as_str().unwrap();
    let foreign_revision = foreign["word"]["revision"].as_i64().unwrap();
    let foreign_lifecycle_revision = foreign["word"]["lifecycle_revision"].as_i64().unwrap();
    let (status, response) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{foreign_id}"),
        &super_bearer,
        None,
        Some(json!({
            "base_revision": foreign_revision,
            "base_lifecycle_revision": foreign_lifecycle_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "超管应能删除他人创建的词条：{response}"
    );
}

#[sqlx::test]
async fn delete_batch_is_atomic_and_idempotent(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 造三条草稿并全部移入垃圾桶。
    let mut archived_targets = Vec::new();
    for index in 0..3 {
        let draft =
            create_ready_draft(&state, &pool, &bearer, &format!("batch-delete-{index}")).await;
        let id = draft["word"]["id"].as_str().unwrap().to_owned();
        let revision = draft["word"]["revision"].as_i64().unwrap();
        let lifecycle_revision = draft["word"]["lifecycle_revision"].as_i64().unwrap();
        let (status, archived) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/entries/{id}/archive"),
            &bearer,
            Some(Uuid::now_v7()),
            Some(json!({
                "base_revision": revision,
                "base_lifecycle_revision": lifecycle_revision
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "归档失败：{archived}");
        archived_targets.push(json!({
            "id": id,
            "base_revision": archived["word"]["revision"].as_i64().unwrap(),
            "base_lifecycle_revision": archived["word"]["lifecycle_revision"].as_i64().unwrap(),
        }));
    }

    // 原子性：批次里混入一条已发布词条，整批必须不生效。
    let published = create_ready_draft(&state, &pool, &bearer, "batch-delete-published").await;
    let (status, published) = publish_ready(&state, &bearer, &published).await;
    assert_eq!(status, StatusCode::CREATED, "发布准备失败：{published}");
    let mut poisoned = archived_targets.clone();
    poisoned.push(json!({
        "id": published["word"]["id"].as_str().unwrap(),
        "base_revision": published["word"]["revision"].as_i64().unwrap(),
        "base_lifecycle_revision": published["word"]["lifecycle_revision"].as_i64().unwrap(),
    }));
    let (status, response) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/delete-batch"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({ "entries": poisoned })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "批次含已发布词条时必须整批拒绝：{response}"
    );
    assert_eq!(response["code"], "entry_not_deletable");
    for target in &archived_targets {
        let id = target["id"].as_str().unwrap();
        let (status, _) = call(
            &state,
            Method::GET,
            &format!("{ROOT}/entries/{id}"),
            &bearer,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "整批拒绝后归档词条必须原样保留");
    }

    // 正常路径 + 幂等：同一幂等键重放返回同一结果，且不重复删除。
    let key = Uuid::now_v7();
    let (status, response) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/delete-batch"),
        &bearer,
        Some(key),
        Some(json!({ "entries": archived_targets })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "批量永久删除应成功：{response}");
    assert_eq!(response["affected"], 3);
    for target in &archived_targets {
        let id = target["id"].as_str().unwrap();
        let (status, _) = call(
            &state,
            Method::GET,
            &format!("{ROOT}/entries/{id}"),
            &bearer,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "批量删除后词条不应再可读");
    }
    let (status, replay) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/delete-batch"),
        &bearer,
        Some(key),
        Some(json!({ "entries": archived_targets })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "幂等重放应成功：{replay}");
    assert_eq!(replay["affected"], 3, "幂等重放必须返回首次结果");
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

    // 垃圾桶清理：归档只是软删除的中间站，从未发布过的归档草稿可以永久删除。
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
        StatusCode::NO_CONTENT,
        "垃圾桶里从未发布的草稿应可永久删除：{response}"
    );
    let (status, response) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{archive_race_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "永久删除后词条不应再可读：{response}"
    );

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
    let (status, draft_replacement) = call(
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
        "草稿 impact 不应被词性与词形类型不匹配阻断：{draft_replacement}"
    );
    assert_eq!(draft_replacement["requires_confirmation"], true);
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
        .arg(surface_snapshot::snapshot_key_for_test(
            Uuid::parse_str(snapshot_id).unwrap(),
        ))
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
        .arg(surface_snapshot::snapshot_key_for_test(
            Uuid::parse_str(&refreshed_snapshot_id).unwrap(),
        ))
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
    let mut context_blocker = pool.begin().await.unwrap();
    LexiconRepository::lock_surface_contexts(&mut context_blocker, &[entry_uuid])
        .await
        .expect("应能建立稳定的 context lock 竞争条件");

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
    let (forms_status, rejected_forms) = call(
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
    let (meanings_status, rejected_meanings) = call(
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
    context_blocker.rollback().await.unwrap();
    assert_eq!(
        forms_status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "forms 超限应返回结构化 422：{rejected_forms}"
    );
    assert_eq!(rejected_forms["code"], "validation_failed");
    assert!(has_issue(&rejected_forms, "aggregate_node_limit_exceeded"));
    assert_eq!(
        meanings_status,
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

/// 语法结构的方言形状（后端提案 P1 · 英美方言偏好化 A1）：distinguish 词条既接受
/// 历史的 uk + us 双条，也接受收敛后的单条 common；unified 词条仍然只接受 common，
/// 缺一侧、多一侧或方言重复都照旧拒绝。
#[sqlx::test]
async fn grammar_structures_accept_a_single_common_variant_on_distinguish_entries(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let ready = create_ready_draft_with_headwords(
        &state,
        &pool,
        &bearer,
        "grammarcolour",
        Some(json!({
            "mode": "distinguish",
            "uk": "grammarcolour",
            "us": "grammarcolor",
            "source_dialect": "uk",
        })),
    )
    .await;
    let entry_id = ready["word"]["id"].as_str().unwrap().to_owned();
    let meanings = ready["word"]["meanings"].clone();
    let template = meanings["pos"][0]["grammar_structures"][0]["variants"][0].clone();
    assert_eq!(
        meanings["pos"][0]["grammar_structures"][0]["variants"]
            .as_array()
            .map(Vec::len),
        Some(2),
        "distinguish 骨架仍应产出 uk + us 双条，本用例的起点即历史形状"
    );

    // 收敛成单条 common 必须换新节点 ID：节点角色里带方言，复用旧 ID 会被判 node_binding_changed。
    let fresh_variant = |dialect: &str| {
        let mut variant = template.clone();
        variant["id"] = json!(Uuid::now_v7());
        variant["dialect"] = json!(dialect);
        variant["content"] = rich_text("used as a noun");
        variant
    };
    // 反过来，重新写回 uk / us 必须沿用骨架里那两个节点 ID——方言槽位一旦建过就固定了。
    let stored_variant = |index: usize| {
        let mut variant = meanings["pos"][0]["grammar_structures"][0]["variants"][index].clone();
        variant["content"] = rich_text("used as a noun");
        variant
    };
    let save = async |variants: Value, revision: i64| {
        let mut content = meanings.clone();
        content["pos"][0]["grammar_structures"][0]["variants"] = variants;
        call(
            &state,
            Method::PUT,
            &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
            &bearer,
            None,
            Some(json!({
                "base_revision": revision,
                "intent": "complete",
                "content": content,
            })),
        )
        .await
    };

    let revision = ready["word"]["revision"].as_i64().expect("应带 revision");
    let (status, converged) = save(json!([fresh_variant("common")]), revision).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "distinguish 词条的单条 common 语法结构应被接受：{converged}"
    );
    let stored = &converged["word"]["meanings"]["pos"][0]["grammar_structures"][0]["variants"];
    assert_eq!(stored.as_array().map(Vec::len), Some(1));
    assert_eq!(stored[0]["dialect"], "common");

    let revision = converged["word"]["revision"]
        .as_i64()
        .expect("应带 revision");
    let (status, rejected) = save(json!([stored_variant(0)]), revision).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert!(
        has_issue(&rejected, "grammar_variants_invalid"),
        "只写单侧 uk 仍应被拒：{rejected}"
    );
    let (status, rejected) = save(json!([stored[0].clone(), stored_variant(0)]), revision).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert!(
        has_issue(&rejected, "grammar_variants_invalid"),
        "common 之外再挂一侧 uk 仍应被拒：{rejected}"
    );

    // unified 词条不跟着放宽：它只有 common 一种合法形状。
    let unified = create_ready_draft(&state, &pool, &bearer, "grammarunified").await;
    let mut content = unified["word"]["meanings"].clone();
    let mut template = content["pos"][0]["grammar_structures"][0]["variants"][0].clone();
    template["content"] = rich_text("used as a noun");
    content["pos"][0]["grammar_structures"][0]["variants"] = json!(
        ["uk", "us"]
            .into_iter()
            .map(|dialect| {
                let mut variant = template.clone();
                variant["id"] = json!(Uuid::now_v7());
                variant["dialect"] = json!(dialect);
                variant
            })
            .collect::<Vec<_>>()
    );
    let (status, rejected) = call(
        &state,
        Method::PUT,
        &format!(
            "{ROOT}/entries/{}/steps/meanings",
            unified["word"]["id"].as_str().unwrap()
        ),
        &bearer,
        None,
        Some(json!({
            "base_revision": unified["word"]["revision"],
            "intent": "complete",
            "content": content,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert!(
        has_issue(&rejected, "grammar_variants_invalid"),
        "unified 词条写 uk + us 双条仍应被拒：{rejected}"
    );
}

/// 整步草稿内容的请求体上限：axum 默认 2 MiB 装不下塞满的词条（2000 节点），
/// 所以这三条路由单独放宽到 MAX_STEP_CONTENT_BODY_BYTES；其余接口维持默认值。
/// 超限只能报 413 payload_too_large——退化成 422 会让前端把「录太多」当成「格式错」。
///
/// 边界逐字节钉死：上限本身必须被接受、上限 +1 必须被拒。对外文档给的是同一个数字，
/// 一旦实现与文档漂移（例如把 2000 × 4 KiB 当成 8 MiB），这里立刻红。
#[sqlx::test]
async fn step_content_body_limit_is_raised_bounded_and_scoped_per_route(pool: PgPool) {
    // 镜像 axum-core 私有的 DEFAULT_LIMIT（ext_traits/request.rs），没有公开 API 可引用。
    // axum 升级后若这个默认值变了，下面「批量接口仍吃默认值」那段会失败——那是依赖漂移，
    // 不是本改动回归。
    const AXUM_DEFAULT_BODY_LIMIT: usize = 2 * 1024 * 1024;

    // 编译期就钉死，不用等测试跑起来：
    // 上限必须真的高于框架默认值，且必须等于文档 §13.2 对外承诺的精确值。
    const {
        assert!(
            MAX_STEP_CONTENT_BODY_BYTES > AXUM_DEFAULT_BODY_LIMIT,
            "放宽必须真的高于框架默认值，否则这条改动没有意义"
        );
        // 这个数字对外散在三处，改了要一起改，否则前端拿到的是旧值：
        //   1. docs/frontend-integration.md §13.2（表格、警告框、TS 常量）
        //   2. src/lexicon/handler/commands.rs 三条路由的 utoipa 413 description
        //   3. 本断言自身
        assert!(
            MAX_STEP_CONTENT_BODY_BYTES == 8_192_000,
            "整步内容上限变了：请同步 frontend-integration.md §13.2、三条路由的 utoipa 413 description，以及本断言"
        );
    }

    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let entry_id = Uuid::now_v7();

    // 形状不合 DTO 但结构完整的 JSON：能读完就是 422，读不完才是 413。
    // 用它把「请求体被完整读入」与「词条是否存在」解耦。
    let envelope = r#"{"not_a_field":""}"#.len();
    let body_of_exactly = |total: usize| {
        assert!(
            total >= envelope,
            "目标字节数至少要装得下 JSON 外壳（{envelope} 字节）"
        );
        let padding = "a".repeat(total - envelope);
        let body = format!(r#"{{"not_a_field":"{padding}"}}"#).into_bytes();
        assert_eq!(body.len(), total, "构造的请求体应恰好是目标字节数");
        body
    };

    // 三条路由都放宽了，三条都要验——只测两条的话，漏挂 layer 的第三条不会红。
    let step_content_routes = [
        (
            Method::PUT,
            format!("{ROOT}/entries/{entry_id}/steps/forms"),
        ),
        (
            Method::PUT,
            format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        ),
        (
            Method::POST,
            format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        ),
    ];

    // 恰好等于上限：必须被完整读入（走到 DTO 反序列化才失败），不能是 413。
    let at_limit = body_of_exactly(MAX_STEP_CONTENT_BODY_BYTES);
    for (method, uri) in &step_content_routes {
        let (status, problem) =
            call_raw(&state, method.clone(), uri, &bearer, None, &at_limit).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "恰好等于上限的请求体应被接受并读完：{uri} → {problem}"
        );
        assert_eq!(problem["code"], "invalid_request_body");
    }

    // 上限 +1：必须 413，且是 payload_too_large 而不是 invalid_request_body。
    let over_limit = body_of_exactly(MAX_STEP_CONTENT_BODY_BYTES + 1);
    for (method, uri) in &step_content_routes {
        let (status, problem) =
            call_raw(&state, method.clone(), uri, &bearer, None, &over_limit).await;
        assert_eq!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "上限 +1 应被拒：{uri} → {problem}"
        );
        assert_eq!(problem["code"], "payload_too_large");
        assert_eq!(problem["type"], "urn:tsz:problem:payload_too_large");
    }

    let over_axum_default = body_of_exactly(AXUM_DEFAULT_BODY_LIMIT + 4_096);

    // 放宽是逐路由的：不承载整步内容的接口仍然吃 axum 默认值。
    let idempotency_key = Uuid::now_v7().to_string();
    let (status, problem) = call_raw(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/restore-batch"),
        &bearer,
        Some(&idempotency_key),
        &over_axum_default,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "批量接口不应跟着放宽：{problem}"
    );
    assert_eq!(problem["code"], "payload_too_large");
}

/// 保存 forms，遇到 surface warning 就补确认 token 重放一次。
///
/// 方言侧切换会改写 surface 投影，本用例关心的是节点身份而不是同形提示，
/// 所以把提示这一步吸收掉，让断言只针对身份契约。
async fn save_forms_step(
    state: &AppState,
    bearer: &str,
    entry_id: &str,
    base_revision: i64,
    content: &Value,
) -> (StatusCode, Value) {
    let input = json!({
        "base_revision": base_revision,
        "intent": "save",
        "content": content,
    });
    let (status, body) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        bearer,
        None,
        Some(input.clone()),
    )
    .await;
    if status != StatusCode::CONFLICT || body["code"] != "surface_match_acknowledgement_required" {
        return (status, body);
    }
    let mut confirmed = input;
    confirmed["confirmed_surface_match_token"] = json!(
        body["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .expect("单页 forms warning 应签发确认 token")
    );
    call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        bearer,
        None,
        Some(confirmed),
    )
    .await
}

/// 把一个词形槽位从单条 common 拆成 uk / us 两条，返回被替换掉的 common 节点 ID。
fn split_slot_variants(slot: &mut Value) -> Value {
    let common = slot["variants"][0].clone();
    slot["variants"] = Value::Array(
        ["uk", "us"]
            .iter()
            .map(|dialect| {
                json!({
                    "id": Uuid::now_v7(),
                    "dialect": dialect,
                    "spelling": common["spelling"],
                    "origin": common["origin"],
                    "pronunciations": [{
                        "id": Uuid::now_v7(),
                        "dict_phonetic": "/split/",
                        "actual_pron": "split",
                        "style": "normal"
                    }]
                })
            })
            .collect(),
    );
    common["id"].clone()
}

/// 把一个词形槽位从 uk / us 合回单条 common，沿用调用方给出的节点 ID。
fn merge_slot_variants(slot: &mut Value, common_id: Value) {
    let uk = slot["variants"][0].clone();
    slot["variants"] = json!([{
        "id": common_id,
        "dialect": "common",
        "spelling": uk["spelling"],
        "origin": uk["origin"],
        "pronunciations": [{
            "id": Uuid::now_v7(),
            "dict_phonetic": "/merged/",
            "actual_pron": "merged",
            "style": "normal"
        }]
    }]);
}

fn retired_slot<'a>(draft: &'a Value, node_id: &Value) -> Option<&'a Value> {
    draft["retired_stable_slots"]
        .as_array()
        .expect("草稿响应应带 retired_stable_slots 数组")
        .iter()
        .find(|slot| slot["id"] == *node_id)
}

fn issue_for_node<'a>(body: &'a Value, node_id: &Value) -> &'a Value {
    body["field_issues"]
        .as_array()
        .expect("422 应带 field_issues")
        .iter()
        .find(|issue| issue["node_id"] == *node_id)
        .unwrap_or_else(|| panic!("缺少节点 {node_id} 的 issue：{body}"))
}

#[sqlx::test]
async fn dialect_split_and_merge_round_trip_reuses_retired_stable_node_ids(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let ready = create_ready_draft(&state, &pool, &bearer, "testability").await;
    let entry_id = ready["word"]["id"].as_str().unwrap().to_owned();
    let unified_forms = ready["word"]["forms"].clone();
    let pos_id = unified_forms["pos"][0]["pos_id"].clone();
    let pos_code = unified_forms["pos"][0]["pos"].clone();
    let base_slot_id = unified_forms["pos"][0]["base_form"]["id"].clone();
    let derived_slot_id = unified_forms["pos"][0]["form_groups"][0]["slots"][0]["id"].clone();
    let derived_form_type =
        unified_forms["pos"][0]["form_groups"][0]["slots"][0]["form_type"].clone();

    // 1. 共用 → 英美区分：common 变体消失，uk / us 带新 ID 出现。
    let mut split_forms = unified_forms.clone();
    split_forms["pos"][0]["dialect_rules"] =
        json!({"spelling_mode": "distinguish", "phonetic_mode": "distinguish"});
    let base_common_id = split_slot_variants(&mut split_forms["pos"][0]["base_form"]);
    let derived_common_id =
        split_slot_variants(&mut split_forms["pos"][0]["form_groups"][0]["slots"][0]);
    let (status, split_saved) = save_forms_step(
        &state,
        &bearer,
        &entry_id,
        ready["word"]["revision"].as_i64().unwrap(),
        &split_forms,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "拆分英美应保存成功：{split_saved}");

    // 2. 刷新页面：草稿本身已看不到 common 变体，身份只能从 retired_stable_slots 找回。
    let (status, draft) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "拆分后应能读回草稿：{draft}");
    assert!(
        !serde_json::to_string(&draft["word"]["forms"])
            .unwrap()
            .contains(base_common_id.as_str().unwrap()),
        "拆分后草稿内容里不应再出现 common 变体：{draft}"
    );
    let retired_base = retired_slot(&draft, &base_common_id)
        .unwrap_or_else(|| panic!("原形 common 变体身份应可找回：{draft}"));
    assert_eq!(retired_base["parent_node_id"], base_slot_id);
    assert_eq!(retired_base["node_role"], "forms.form_variant:common");
    let retired_derived = retired_slot(&draft, &derived_common_id)
        .unwrap_or_else(|| panic!("派生词形 common 变体身份应可找回：{draft}"));
    assert_eq!(retired_derived["parent_node_id"], derived_slot_id);
    assert_eq!(retired_derived["node_role"], "forms.form_variant:common");

    // 3. 英美区分 → 共用：沿用找回的 common ID 必须放行。
    let mut merged_forms = draft["word"]["forms"].clone();
    merged_forms["pos"][0]["dialect_rules"] =
        json!({"spelling_mode": "unified", "phonetic_mode": "unified"});
    merge_slot_variants(
        &mut merged_forms["pos"][0]["base_form"],
        retired_base["id"].clone(),
    );
    merge_slot_variants(
        &mut merged_forms["pos"][0]["form_groups"][0]["slots"][0],
        retired_derived["id"].clone(),
    );
    let (status, merged) = save_forms_step(
        &state,
        &bearer,
        &entry_id,
        draft["word"]["revision"].as_i64().unwrap(),
        &merged_forms,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "沿用退役身份合回共用应保存成功：{merged}"
    );
    assert_eq!(
        merged["word"]["forms"]["pos"][0]["base_form"]["variants"][0]["id"],
        base_common_id
    );

    // 合回之后换成退役的是 uk / us 两侧，common 重新在用。
    let (status, remerged_draft) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "合并后应能读回草稿：{remerged_draft}"
    );
    assert!(
        retired_slot(&remerged_draft, &base_common_id).is_none(),
        "重新在用的槽位不应留在退役清单里：{remerged_draft}"
    );
    assert!(
        remerged_draft["retired_stable_slots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|slot| slot["node_role"] == "forms.form_variant:uk"),
        "退役的 uk 变体应进入清单：{remerged_draft}"
    );

    // 4. 已有槽位换新 ID 仍然拒绝，并且带得出界面位置。
    let mut rebound_forms = merged["word"]["forms"].clone();
    let rebound_base_id = json!(Uuid::now_v7());
    let rebound_derived_id = json!(Uuid::now_v7());
    rebound_forms["pos"][0]["base_form"]["variants"][0]["id"] = rebound_base_id.clone();
    rebound_forms["pos"][0]["form_groups"][0]["slots"][0]["variants"][0]["id"] =
        rebound_derived_id.clone();
    let (status, rejected) = save_forms_step(
        &state,
        &bearer,
        &entry_id,
        merged["word"]["revision"].as_i64().unwrap(),
        &rebound_forms,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "已有槽位换新 ID 应拒绝：{rejected}"
    );
    assert!(has_issue(&rejected, "stable_node_id_changed"));

    let base_issue = issue_for_node(&rejected, &rebound_base_id);
    assert_eq!(base_issue["code"], "stable_node_id_changed");
    let base_location = &base_issue["node_location"];
    assert_eq!(base_location["node_role"], "forms.form_variant:common");
    assert_eq!(base_location["pos"], pos_code);
    assert_eq!(base_location["pos_id"], pos_id);
    assert_eq!(base_location["form_type"], "base");
    assert_eq!(base_location["dialect"], "common");
    assert_eq!(
        base_location["ancestor_node_ids"],
        json!([pos_id, base_slot_id])
    );
    assert!(
        base_location["form_group_index"].is_null(),
        "共享原形不属于任何词形组：{base_issue}"
    );

    let derived_issue = issue_for_node(&rejected, &rebound_derived_id);
    let derived_location = &derived_issue["node_location"];
    assert_eq!(derived_location["form_type"], derived_form_type);
    assert_eq!(derived_location["form_group_index"], 0);
    assert_eq!(derived_location["dialect"], "common");
    assert_eq!(
        derived_location["ancestor_node_ids"],
        json!([
            pos_id,
            merged["word"]["forms"]["pos"][0]["form_groups"][0]["id"],
            derived_slot_id
        ])
    );
    assert!(
        !serde_json::to_string(base_issue)
            .unwrap()
            .contains(base_common_id.as_str().unwrap()),
        "定位信息不应回传存量节点 ID：{base_issue}"
    );
}

/// 建一个「只完成 basics」的草稿：词形与词义都还没做完，用于验证跨步骤跳转。
async fn create_incomplete_draft(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
    headword: &str,
) -> Value {
    seed_dictionary_word(pool, headword).await;
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
        "headwords": detection["builtin_dictionary"]["headwords"].clone(),
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
    created
}

#[sqlx::test]
async fn meanings_step_accepts_draft_before_forms_step_completes(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let headword = format!("draftfirst{}", admin_id.simple());
    let created = create_incomplete_draft(&state, &pool, &bearer, &headword).await;
    let entry_id = created["word"]["id"].as_str().unwrap().to_owned();

    fn completed(word: &Value) -> Vec<String> {
        word["completed_steps"]
            .as_array()
            .expect("completed_steps 应是数组")
            .iter()
            .filter_map(|step| step.as_str().map(str::to_owned))
            .collect()
    }

    // 音标还没查，词形步本就未完成。
    assert!(
        !completed(&created["word"]).contains(&"forms".to_owned()),
        "新建草稿的词形步不应是完成态：{created}"
    );

    // 放宽点：词形步未完成，也能把现成的词义资料先存成草稿。
    let mut meanings = created["word"]["meanings"].clone();
    meanings["sense_groups"][0]["name_zh"] = json!("先录的含义");
    meanings["sense_groups"][0]["name_en"] = json!("Draft meaning");
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": created["word"]["revision"],
            "intent": "save",
            "content": meanings.clone(),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "词形步未完成时保存词义草稿应成功：{saved}"
    );
    assert_eq!(
        saved["word"]["meanings"]["sense_groups"][0]["name_zh"], "先录的含义",
        "词义草稿内容应被存下：{saved}"
    );

    // 完成情况面板必须如实：存词义不得把词形步谎报成已完成。
    assert!(
        !completed(&saved["word"]).contains(&"forms".to_owned()),
        "保存词义不应把词形步标记为完成：{saved}"
    );
    assert!(
        !completed(&saved["word"]).contains(&"meanings".to_owned()),
        "intent=save 不应把词义步标记为完成：{saved}"
    );
    assert_eq!(
        saved["word"]["max_reachable_step"], "forms",
        "词形步未完成时「继续创建」落点应停在词形步：{saved}"
    );

    // 保存响应里的落点/完成情况必须与重新读取时的派生结果一致。
    let (status, reloaded) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "读取词条失败：{reloaded}");
    assert_eq!(
        reloaded["word"]["max_reachable_step"], saved["word"]["max_reachable_step"],
        "保存响应与重新读取的落点必须一致：{reloaded}"
    );
    assert_eq!(
        completed(&reloaded["word"]),
        completed(&saved["word"]),
        "保存响应与重新读取的完成情况必须一致：{reloaded}"
    );

    // 「标记完成」不放宽：词形步没完成就不能把词义步标记为完成。
    let (status, completing) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": saved["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "词形步未完成时标记词义步完成应被拒绝：{completing}"
    );
    assert_eq!(completing["code"], "step_not_reachable");

    // 发布才是真正的守门人：内容不全仍必须被完整性校验挡下，且要独立重跑词形校验。
    let (status, published) = publish_ready_confirming(&state, &bearer, &saved).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "内容不全的词条不应发布成功：{published}"
    );
    assert_eq!(published["code"], "validation_failed");
    assert!(
        published["field_issues"]
            .as_array()
            .is_some_and(|issues| issues.iter().any(|issue| issue["step"] == "forms")),
        "发布应独立重跑词形校验并报出词形问题：{published}"
    );
}

#[sqlx::test]
async fn meanings_step_without_any_part_of_speech_stores_shell_but_rejects_dangling_pos(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let headword = format!("noposyet{}", admin_id.simple());
    let created = create_incomplete_draft(&state, &pool, &bearer, &headword).await;
    let entry_id = created["word"]["id"].as_str().unwrap().to_owned();
    let dangling_pos_id = created["word"]["meanings"]["pos"][0]["pos_id"].clone();
    assert!(
        dangling_pos_id.is_string(),
        "骨架应自带一个基本词性：{created}"
    );

    // 把词性全部删光——`pos_required` 不阻断 save，所以这是个可达的草稿状态。
    let mut forms = created["word"]["forms"].clone();
    forms["pos"] = json!([]);
    let (status, forms_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": created["word"]["revision"],
            "intent": "save",
            "content": forms,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "清空词性的词形草稿应能保存：{forms_saved}"
    );
    assert_eq!(
        forms_saved["word"]["meanings"]["pos"],
        json!([]),
        "词性删光后词义侧的 pos 应被一并收敛：{forms_saved}"
    );

    // 一个基本词性都没有时，不带 pos 的词义空壳仍可保存（前端第 3 步空态据此设计）。
    let (status, shell_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": forms_saved["word"]["revision"],
            "intent": "save",
            "content": forms_saved["word"]["meanings"].clone(),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "零词性时保存不含 pos 的词义空壳应成功：{shell_saved}"
    );

    // 但引用了 forms 里不存在的 pos_id，必须被存储层安全网挡下，与 intent 无关。
    let mut dangling = shell_saved["word"]["meanings"].clone();
    dangling["pos"] = json!([{
        "pos_id": dangling_pos_id,
        "grammar_structures": [],
        "senses": []
    }]);
    let (status, rejected) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": shell_saved["word"]["revision"],
            "intent": "save",
            "content": dangling,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "词义引用不存在的基本词性必须被拒绝：{rejected}"
    );
    assert!(
        has_issue(&rejected, "pos_not_found"),
        "应报出 pos_not_found：{rejected}"
    );
}

#[sqlx::test]
async fn meanings_drafted_before_forms_survive_completing_forms_and_reach_publish(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let headword = format!("meanfirst{}", admin_id.simple());
    let created = create_incomplete_draft(&state, &pool, &bearer, &headword).await;
    let entry_id = created["word"]["id"].as_str().unwrap().to_owned();

    // 第 1 步：音标还没查，先把手上现成的词义资料录进去。
    let mut meanings = created["word"]["meanings"].clone();
    meanings["sense_groups"][0]["name_zh"] = json!("先录的语义区间");
    meanings["sense_groups"][0]["name_en"] = json!("Drafted group");
    let grammar_variants = meanings["pos"][0]["grammar_structures"][0]["variants"]
        .as_array()
        .expect("语法结构应带方言行")
        .len();
    for index in 0..grammar_variants {
        meanings["pos"][0]["grammar_structures"][0]["variants"][index]["content"] =
            rich_text("used as a noun");
    }
    meanings["pos"][0]["senses"][0]["sub_pos"] = json!("N-COUNT");
    meanings["pos"][0]["senses"][0]["frequency"] = json!("50");
    meanings["pos"][0]["senses"][0]["definitions"][0]["content"] = rich_text("先录进来的释义");
    let example = rich_text("A drafted example.");
    let en_text = &mut meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"];
    if en_text["mode"] == "distinguish" {
        for side in ["uk", "us"] {
            if en_text[side]["state"] == "ready" {
                en_text[side]["variant"]["value"] = example.clone();
            } else {
                en_text[side] = json!({
                    "state": "ready",
                    "variant": {
                        "id": Uuid::now_v7(),
                        "value": example.clone(),
                        "origin": "manual"
                    }
                });
            }
        }
    } else {
        en_text["common"]["value"] = example;
    }
    meanings["pos"][0]["senses"][0]["sentences"][0]["zh_text"] = rich_text("先录进来的例句。");

    let (status, meanings_drafted) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": created["word"]["revision"],
            "intent": "save",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "词形未完成时应能先录词义：{meanings_drafted}"
    );

    // 第 2 步：查到音标了，回头补完词形。
    let mut forms = meanings_drafted["word"]["forms"].clone();
    let base_variants = forms["pos"][0]["base_form"]["variants"]
        .as_array()
        .expect("基本形应带方言行")
        .clone();
    for index in 0..base_variants.len() {
        let pronunciation =
            &mut forms["pos"][0]["base_form"]["variants"][index]["pronunciations"][0];
        pronunciation["dict_phonetic"] = json!("/test/");
        pronunciation["actual_pron"] = json!("test");
    }
    forms["pos"][0]["form_groups"][0]["slots"] = json!([{
        "id": Uuid::now_v7(),
        "form_type": "plural",
        "variants": base_variants
            .iter()
            .map(|variant| json!({
                "id": Uuid::now_v7(),
                "dialect": variant["dialect"],
                "spelling": format!("{}s", variant["spelling"].as_str().unwrap()),
                "origin": "manual",
                "pronunciations": [{
                    "id": Uuid::now_v7(),
                    "dict_phonetic": "/tests/",
                    "actual_pron": "tests",
                    "style": "normal"
                }]
            }))
            .collect::<Vec<_>>()
    }]);
    let forms_input = json!({
        "base_revision": meanings_drafted["word"]["revision"],
        "intent": "complete",
        "content": forms,
    });
    let (mut status, mut forms_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
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
            &state,
            Method::PUT,
            &format!("{ROOT}/entries/{entry_id}/steps/forms"),
            &bearer,
            None,
            Some(confirmed),
        )
        .await;
    }
    assert_eq!(status, StatusCode::OK, "补完词形应成功：{forms_saved}");

    // 补词形不得冲掉先录进去的词义内容。
    assert_eq!(
        forms_saved["word"]["meanings"]["sense_groups"][0]["name_zh"], "先录的语义区间",
        "补词形后语义区间名应保留：{forms_saved}"
    );
    assert_eq!(
        forms_saved["word"]["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"],
        "先录进来的释义",
        "补词形后先录的释义应保留：{forms_saved}"
    );
    assert_eq!(
        forms_saved["word"]["meanings"]["pos"][0]["senses"][0]["sub_pos"], "N-COUNT",
        "补词形后先录的子词性应保留：{forms_saved}"
    );

    // 第 3 步：词形完成后，词义步才能标记完成，然后正常发布。
    let (status, meanings_done) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": forms_saved["word"]["revision"],
            "intent": "complete",
            "content": forms_saved["word"]["meanings"].clone(),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "词形完成后标记词义完成应成功：{meanings_done}"
    );
    assert_eq!(
        meanings_done["word"]["max_reachable_step"], "preview",
        "两步都完成后应能到预览：{meanings_done}"
    );

    let (status, published) = publish_ready_confirming(&state, &bearer, &meanings_done).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "先录词义、后补音标的词条应能正常发布：{published}"
    );
}

/// 非英文词条曾能一路落库成 `language = "en"` 的脏数据：内置词典未命中时创建路径放行，
/// 而字符集只在 admin 前端拦，任何持 admin token 的调用方都能绕过。检测与创建两个入口
/// 各自兜底一次——检测拦住误操作，创建拦住直接调 API。
#[sqlx::test]
async fn non_english_headwords_are_rejected_by_detection_and_creation(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis.clone())
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    for headword in ["苹果测试", "apple苹果", "りんご", "яблоко", "123456", "---"] {
        let (status, problem) = call(
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
            StatusCode::BAD_REQUEST,
            "{headword} 本不该通过检测：{problem}"
        );
        assert_eq!(problem["code"], "invalid_headword");
        assert_eq!(problem["field"], "headword");
    }

    for surface in ["苹果测试", "apple苹果", "りんご", "яблоко", "123456", "---"] {
        let (status, problem) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/detections"),
            &bearer,
            None,
            Some(json!({
                "schema_version": 3,
                "language": "en",
                "kind": "word",
                "surface": surface,
            })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "V3 surface {surface} 本不该通过检测：{problem}"
        );
        assert_eq!(problem["code"], "invalid_headword");
        assert_eq!(problem["field"], "surface");
    }

    let (status, valid_v3_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "café",
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "合法 V3 surface 检测失败：{valid_v3_detection}"
    );
    let mut tampered_v3_detection: DetectLexiconSurfaceResponseV3 =
        serde_json::from_value(valid_v3_detection).unwrap();
    tampered_v3_detection.request.surface = "苹果测试".to_owned();
    tampered_v3_detection.normalized_surface = "苹果测试".to_owned();
    DetectionStore::new(redis)
        .save_v3(admin_id, &tampered_v3_detection, Duration::from_secs(300))
        .await
        .unwrap();
    let entries_before: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    let (status, problem) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": tampered_v3_detection.detection_id,
            "kind": "word",
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "历史非法 V3 detection 本不该创建成功：{problem}"
    );
    assert_eq!(problem["code"], "invalid_headword");
    assert_eq!(problem["field"], "surface");
    let entries_after: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        entries_after, entries_before,
        "非法 V3 detection 不得写入 entry"
    );

    // 字符集只拦非英文，不收紧 not_found 的创建口子：生造词、变音符、缩写仍要放行。
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "café"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "合法英文词条检测失败：{detection}");
    assert_eq!(detection["builtin_dictionary"]["status"], "not_found");

    // 创建入口独立兜底：持一个合法检测也不能把非英文主词塞进来（先于检测证据比对触发，
    // 所以拿到的是 invalid_headword 而不是 detection_mismatch）。
    let (status, problem) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 2,
            "detection_id": detection["detection_id"],
            "headwords": {"mode": "unified", "common": "苹果测试"},
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "非英文主词本不该创建成功：{problem}"
    );
    assert_eq!(problem["code"], "invalid_headword");

    // distinguish 模式的英美主词各解析一次，逐侧都要拦：只测一侧的话，另一侧漏掉
    // 校验时测试仍会全绿。非法侧先于检测证据比对触发，所以拿到的是 invalid_headword。
    for (label, headwords) in [
        (
            "英式侧",
            json!({
                "mode": "distinguish",
                "uk": "苹果测试",
                "us": "café",
                "source_dialect": "us",
            }),
        ),
        (
            "美式侧",
            json!({
                "mode": "distinguish",
                "uk": "café",
                "us": "苹果测试",
                "source_dialect": "uk",
            }),
        ),
    ] {
        let (status, problem) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/entries"),
            &bearer,
            Some(Uuid::now_v7()),
            Some(json!({
                "schema_version": 2,
                "detection_id": detection["detection_id"],
                "headwords": headwords,
            })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{label}非英文主词本不该创建成功：{problem}"
        );
        assert_eq!(problem["code"], "invalid_headword");
        assert_eq!(problem["field"], "headword");
    }

    let mut create_input = json!({
        "schema_version": 2,
        "detection_id": detection["detection_id"],
        "headwords": {"mode": "unified", "common": "café"},
    });
    if let Some(surface_token) =
        detection["smart_dictionary"]["surface_match_page"]["surface_confirmation_token"].as_str()
    {
        create_input["confirmed_surface_match_token"] = json!(surface_token);
    }
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(create_input),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "合法英文词条创建失败：{created}"
    );
    assert_eq!(created["word"]["headwords"]["common"], "café");
}

#[sqlx::test]
async fn publishing_resolves_sentence_words_to_the_single_published_sense(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target = create_and_publish(&state, &pool, &bearer, "wall").await;
    let target_entry_id = target["word"]["id"].as_str().unwrap().to_owned();
    let target_sense_id = target["word"]["meanings"]["pos"][0]["senses"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let target_base_slot_id = target["word"]["forms"]["pos"][0]["base_form"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let source = create_ready_draft(&state, &pool, &bearer, "picture").await;
    // 草稿期不处理关联：正文存下去，句中的词一条关联都不产生。
    let saved =
        save_example_sentence(&state, &bearer, &source, "Center the picture on the wall.").await;
    assert_eq!(first_sentence(&saved)["associations"], json!([]));
    assert_eq!(first_sentence(&saved)["associations_state"], "unresolved");
    let draft_associations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.sentence_associations")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(draft_associations, 0, "草稿期不该落任何关联");

    let (status, published) = publish_ready(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");

    let sentence = first_sentence(&published).clone();
    assert_eq!(sentence["associations_state"], "resolved");
    let associations = sentence["associations"].as_array().unwrap();
    // wall 关联上；center / the / on / picture 都不关联——前两类词库里没有，
    // on 是虚词，picture 是词条自己。
    assert_eq!(associations.len(), 1, "{associations:?}");
    let association = &associations[0];
    assert_eq!(association["source_dialect"], "common");
    assert_eq!(association["source_range"]["start"], 26);
    assert_eq!(association["source_range"]["end"], 30);
    assert_eq!(association["source_range"]["surface"], "wall");
    assert_eq!(association["target_word_id"], target_entry_id.as_str());
    assert_eq!(association["target_sense_id"], target_sense_id.as_str());
    assert_eq!(
        association["target_form_slot_id"],
        target_base_slot_id.as_str()
    );
    assert_eq!(association["origin"], "auto");
    assert_eq!(association["resolved_pos"], "noun");
    assert_eq!(association["resolved_form_type"], "base");
    assert_eq!(association["target_headword"], "wall");

    // 读回一致。
    let entry_id = published["word"]["id"].as_str().unwrap();
    let (status, fetched) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        first_sentence(&fetched)["associations"],
        sentence["associations"]
    );
    assert_eq!(first_sentence(&fetched)["associations_state"], "resolved");

    // 发布快照不带只读投影：关联只有库表一份真相。
    let snapshot: Value = sqlx::query_scalar(
        "SELECT snapshot FROM lexicon.entry_publications WHERE entry_id = $1::uuid",
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        snapshot["meanings"]["pos"][0]["senses"][0]["sentences"][0]["associations"],
        json!([])
    );
}

#[sqlx::test]
async fn sentence_associations_are_position_wise_and_publish_survives_every_skip(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    create_and_publish(&state, &pool, &bearer, "wall").await;
    let source = create_ready_draft(&state, &pool, &bearer, "picture").await;
    let saved = save_example_sentence(
        &state,
        &bearer,
        &source,
        "A wall behind the unknownium wall.",
    )
    .await;
    let (status, published) = publish_ready(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "跳词不该影响发布：{published}");

    let associations = first_sentence(&published)["associations"]
        .as_array()
        .unwrap()
        .clone();
    // 同一拼写出现两次 → 两条位置不同的关联；库里没有的词与虚词不产生关联。
    assert_eq!(associations.len(), 2, "{associations:?}");
    assert_eq!(associations[0]["source_range"]["start"], 2);
    assert_eq!(associations[1]["source_range"]["start"], 29);
    assert!(
        associations
            .iter()
            .all(|association| association["source_range"]["surface"] == "wall")
    );
}

/// 复制一个词义（连同其下的释义与例句）并换上全新节点 ID，用来把目标词条做成多义词。
fn duplicate_sense(sense: &Value, entry_id: &str) -> Value {
    let mut clone = sense.clone();
    let sense_id = Uuid::now_v7().to_string();
    clone["id"] = json!(sense_id);
    for definition in clone["definitions"].as_array_mut().unwrap() {
        definition["id"] = json!(Uuid::now_v7().to_string());
        if definition.get("content_id").is_some() {
            definition["content_id"] = json!(Uuid::now_v7().to_string());
        }
    }
    for sentence in clone["sentences"].as_array_mut().unwrap() {
        sentence["id"] = json!(Uuid::now_v7().to_string());
        sentence["zh_text_id"] = json!(Uuid::now_v7().to_string());
        sentence["associations"] = json!([]);
        let en_text = &mut sentence["en_text"];
        if en_text["mode"] == "distinguish" {
            for side in ["uk", "us"] {
                en_text[side]["variant"]["id"] = json!(Uuid::now_v7().to_string());
            }
        } else {
            en_text["common"]["id"] = json!(Uuid::now_v7().to_string());
        }
        for link in sentence["links"].as_array_mut().unwrap() {
            if link["role"] == "focus" {
                link["word_id"] = json!(entry_id);
                link["sense_id"] = json!(sense_id);
            }
        }
    }
    clone
}

#[sqlx::test]
async fn ambiguous_targets_are_skipped_without_failing_the_publish(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 同一词性下有两个词义的目标：句中的词形指不到唯一义项，只能跳过。
    let target = create_ready_draft(&state, &pool, &bearer, "wall").await;
    let target_entry_id = target["word"]["id"].as_str().unwrap().to_owned();
    let mut meanings = target["word"]["meanings"].clone();
    let extra = duplicate_sense(&meanings["pos"][0]["senses"][0], &target_entry_id);
    meanings["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .push(extra);
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": target["word"]["revision"],
            "intent": "complete",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (status, published_target) = publish_ready(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published_target}");
    assert_eq!(
        published_target["word"]["meanings"]["pos"][0]["senses"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let source = create_ready_draft(&state, &pool, &bearer, "picture").await;
    let saved = save_example_sentence(&state, &bearer, &source, "A wall here.").await;
    let (status, published) = publish_ready(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");

    let sentence = first_sentence(&published);
    assert_eq!(
        sentence["associations_state"], "resolved",
        "解析跑过了，只是这个词没有唯一义项可指"
    );
    assert_eq!(sentence["associations"], json!([]));
}

#[sqlx::test]
async fn changing_sentence_text_defers_reparsing_to_the_next_publish(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let wall = create_and_publish(&state, &pool, &bearer, "wall").await;
    let table = create_and_publish(&state, &pool, &bearer, "table").await;
    let table_entry_id = table["word"]["id"].as_str().unwrap().to_owned();
    let table_sense_id = table["word"]["meanings"]["pos"][0]["senses"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let source = create_ready_draft(&state, &pool, &bearer, "picture").await;
    let saved = save_example_sentence(&state, &bearer, &source, "A wall here.").await;
    let (status, published) = publish_ready(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_eq!(
        first_sentence(&published)["associations"][0]["target_word_id"],
        wall["word"]["id"]
    );

    // 改正文后，关联进入 unresolved 且清空，重新解析推迟到下一次发布。
    let changed = save_example_sentence(&state, &bearer, &published, "A table here.").await;
    assert_eq!(first_sentence(&changed)["associations_state"], "unresolved");
    assert_eq!(first_sentence(&changed)["associations"], json!([]));
    let (status, republished) = publish_ready(&state, &bearer, &changed).await;
    assert_eq!(status, StatusCode::CREATED, "{republished}");
    let reparsed = first_sentence(&republished)["associations"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(reparsed.len(), 1, "{reparsed:?}");
    assert_eq!(reparsed[0]["source_range"]["surface"], "table");
    assert_eq!(reparsed[0]["target_word_id"], table_entry_id.as_str());
    assert_eq!(reparsed[0]["target_sense_id"], table_sense_id.as_str());
}

#[sqlx::test]
async fn distinguish_sentences_anchor_associations_to_each_dialect_side(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let wall = create_and_publish(&state, &pool, &bearer, "wall").await;
    let wall_entry_id = wall["word"]["id"].as_str().unwrap().to_owned();

    let source = create_ready_draft_with_headwords(
        &state,
        &pool,
        &bearer,
        "centre",
        Some(json!({
            "mode": "distinguish",
            "uk": "centre",
            "us": "center",
            "source_dialect": "uk",
        })),
    )
    .await;
    let entry_id = source["word"]["id"].as_str().unwrap().to_owned();
    let mut meanings = source["word"]["meanings"].clone();
    // 两侧正文的拼写长度不同，同一个词的下标必然错开——位置只能绑到具体一侧。
    meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"]["uk"]["variant"]["value"] =
        rich_text("A colour wall.");
    meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"]["us"]["variant"]["value"] =
        rich_text("A color wall.");
    let (status, saved) = call(
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
    assert_eq!(status, StatusCode::OK, "{saved}");

    let (status, published) = publish_ready(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    let sentence = first_sentence(&published);
    assert_eq!(sentence["associations_state"], "resolved");
    let associations = sentence["associations"].as_array().unwrap();
    assert_eq!(associations.len(), 2, "{associations:?}");
    assert_eq!(associations[0]["source_dialect"], "uk");
    assert_eq!(associations[0]["source_range"]["start"], 9);
    assert_eq!(associations[1]["source_dialect"], "us");
    assert_eq!(associations[1]["source_range"]["start"], 8);
    assert!(
        associations
            .iter()
            .all(
                |association| association["target_word_id"] == wall_entry_id.as_str()
                    && association["source_range"]["surface"] == "wall"
            )
    );
}

#[sqlx::test]
async fn one_surface_owned_by_two_entries_is_left_unlinked(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // wall 的复数是 walls，而 walls 自己也是一个词条的基本形——同一个词面落在两个词条上，
    // 句中出现时指不到唯一词条，只能跳过。
    let wall = create_ready_draft(&state, &pool, &bearer, "wall").await;
    let (status, published) = publish_ready_confirming(&state, &bearer, &wall).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    let walls = create_ready_draft(&state, &pool, &bearer, "walls").await;
    let (status, published) = publish_ready_confirming(&state, &bearer, &walls).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");

    // 先确认歧义确实成立：两个不同词条都在当前发布版本里认领了 walls 这个词面。
    let owners: i64 = sqlx::query_scalar(
        r#"
        SELECT count(DISTINCT source.entry_id)
        FROM lexicon.surface_sources source
        JOIN lexicon.entries entry
          ON entry.id = source.entry_id
         AND entry.current_publication_id = source.publication_id
        WHERE source.normalized_surface = 'walls'
          AND source.content_scope = 'current_publication'
          AND source.source_kind = 'form'
          AND source.is_deleted = FALSE
        "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owners, 2, "测试前提：walls 必须同时属于两个已发布词条");

    let source = create_ready_draft(&state, &pool, &bearer, "picture").await;
    let saved = save_example_sentence(&state, &bearer, &source, "Two walls stand here.").await;
    let (status, published) = publish_ready_confirming(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");

    let sentence = first_sentence(&published);
    assert_eq!(sentence["associations_state"], "resolved");
    assert_eq!(sentence["associations"], json!([]));
}

#[sqlx::test]
async fn a_surface_shared_by_several_slots_of_one_pos_links_without_guessing_the_slot(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 不规则动词：原形与过去式同拼写。词义仍唯一，关联该建；但槽位没有证据，
    // 不能按变体 ID 顺序随手挑一个。
    let target = create_ready_draft(&state, &pool, &bearer, "cut").await;
    let target_entry_id = target["word"]["id"].as_str().unwrap().to_owned();
    let mut forms = target["word"]["forms"].clone();
    // 只改拼写，槽位与变体的节点 ID 必须原样保留（stable_node_id_changed 会拦）。
    let base_spellings = forms["pos"][0]["base_form"]["variants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|variant| variant["spelling"].clone())
        .collect::<Vec<_>>();
    for (index, spelling) in base_spellings.into_iter().enumerate() {
        forms["pos"][0]["form_groups"][0]["slots"][0]["variants"][index]["spelling"] = spelling;
    }
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "base_revision": target["word"]["revision"],
            "intent": "complete",
            "content": forms,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (status, published_target) = publish_ready_confirming(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published_target}");

    let source = create_ready_draft(&state, &pool, &bearer, "picture").await;
    let saved = save_example_sentence(&state, &bearer, &source, "A cut here.").await;
    let (status, published) = publish_ready_confirming(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");

    let associations = first_sentence(&published)["associations"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(associations.len(), 1, "{associations:?}");
    assert_eq!(associations[0]["target_word_id"], target_entry_id.as_str());
    assert_eq!(associations[0]["resolved_pos"], "noun");
    assert!(
        associations[0].get("target_form_slot_id").is_none(),
        "槽位没有证据时应缺省，而不是按变体 ID 猜一个：{associations:?}"
    );
    assert!(associations[0].get("resolved_form_type").is_none());
}

#[sqlx::test]
async fn dropping_one_dialect_side_stops_serving_that_side_s_associations(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    create_and_publish(&state, &pool, &bearer, "wall").await;
    let source = create_ready_draft_with_headwords(
        &state,
        &pool,
        &bearer,
        "centre",
        Some(json!({
            "mode": "distinguish",
            "uk": "centre",
            "us": "center",
            "source_dialect": "uk",
        })),
    )
    .await;
    let entry_id = source["word"]["id"].as_str().unwrap().to_owned();
    let mut meanings = source["word"]["meanings"].clone();
    for side in ["uk", "us"] {
        meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"][side]["variant"]["value"] =
            rich_text("A wall here.");
    }
    let (status, saved) = call(
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
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (status, published) = publish_ready(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_eq!(
        first_sentence(&published)["associations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // 把 us 侧撤回 missing。uk 侧正文没变，整条例句仍算已解析；但 us 侧的历史关联
    // 指向一份前端已经渲染不出来的正文，不该再出现在响应里。
    let mut meanings = published["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"]["us"] = json!({"state": "missing"});
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": published["word"]["revision"],
            "intent": "save",
            "content": meanings,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let sentence = first_sentence(&saved);
    assert_eq!(sentence["associations_state"], "resolved");
    let associations = sentence["associations"].as_array().unwrap();
    assert_eq!(associations.len(), 1, "{associations:?}");
    assert_eq!(associations[0]["source_dialect"], "uk");
    // 库里那条 us 关联还在，等下一次发布 prune；只是不再对外返回。
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.sentence_associations WHERE source_dialect = 'us'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, 1);
}

#[sqlx::test]
async fn v3_persists_all_three_pos_dialect_rule_combinations(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let surface = format!("v3rules{}", admin_id.simple());
    seed_dictionary_word(&pool, &surface).await;
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let entry_id = Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap();

    let uu_pos_id = Uuid::now_v7();
    let uu_form_id = Uuid::now_v7();
    let ud_pos_id = Uuid::now_v7();
    let ud_form_id = Uuid::now_v7();
    let dd_pos_id = Uuid::now_v7();
    let dd_form_id = Uuid::now_v7();
    let content = json!({
        "pos": [{
            "pos_id": uu_pos_id,
            "pos": "noun",
            "dialect_rules": {
                "spelling_mode": "unified",
                "phonetic_mode": "unified"
            },
            "forms": [{
                "id": uu_form_id,
                "form_type": "base",
                "regional_variants": {
                    "mode": "common",
                    "common": {
                        "id": Uuid::now_v7(),
                        "dialect": "common",
                        "spelling": format!("{surface}-uu"),
                        "origin": "manual",
                        "pronunciations": [{
                            "id": Uuid::now_v7(),
                            "dict_phonetic": "/uu/",
                            "actual_pron": "uu",
                            "style": "normal"
                        }]
                    }
                }
            }],
            "form_groups": [{
                "id": Uuid::now_v7(),
                "is_regular": true,
                "members": [{"id": Uuid::now_v7(), "form_id": uu_form_id}]
            }]
        }, {
            "pos_id": ud_pos_id,
            "pos": "verb",
            "dialect_rules": {
                "spelling_mode": "unified",
                "phonetic_mode": "distinguish"
            },
            "forms": [{
                "id": ud_form_id,
                "form_type": "base",
                "regional_variants": {
                    "mode": "uk_us",
                    "uk": {
                        "id": Uuid::now_v7(),
                        "dialect": "uk",
                        "spelling": format!("{surface}-ud"),
                        "origin": "manual",
                        "pronunciations": [{
                            "id": Uuid::now_v7(),
                            "dict_phonetic": "/ud-uk/",
                            "actual_pron": "ud-uk",
                            "style": "normal"
                        }]
                    },
                    "us": {
                        "id": Uuid::now_v7(),
                        "dialect": "us",
                        "spelling": format!("{surface}-ud"),
                        "origin": "manual",
                        "pronunciations": [{
                            "id": Uuid::now_v7(),
                            "dict_phonetic": "/ud-us/",
                            "actual_pron": "ud-us",
                            "style": "normal"
                        }]
                    }
                }
            }],
            "form_groups": [{
                "id": Uuid::now_v7(),
                "is_regular": true,
                "members": [{"id": Uuid::now_v7(), "form_id": ud_form_id}]
            }]
        }, {
            "pos_id": dd_pos_id,
            "pos": "adjective",
            "dialect_rules": {
                "spelling_mode": "distinguish",
                "phonetic_mode": "distinguish"
            },
            "forms": [{
                "id": dd_form_id,
                "form_type": "base",
                "regional_variants": {
                    "mode": "uk_us",
                    "uk": {
                        "id": Uuid::now_v7(),
                        "dialect": "uk",
                        "spelling": format!("{surface}-dd-uk"),
                        "origin": "manual",
                        "pronunciations": [{
                            "id": Uuid::now_v7(),
                            "dict_phonetic": "/dd-uk/",
                            "actual_pron": "dd-uk",
                            "style": "normal"
                        }]
                    },
                    "us": {
                        "id": Uuid::now_v7(),
                        "dialect": "us",
                        "spelling": format!("{surface}-dd-us"),
                        "origin": "manual",
                        "pronunciations": [{
                            "id": Uuid::now_v7(),
                            "dict_phonetic": "/dd-us/",
                            "actual_pron": "dd-us",
                            "style": "normal"
                        }]
                    }
                }
            }],
            "form_groups": [{
                "id": Uuid::now_v7(),
                "is_regular": false,
                "members": [{"id": Uuid::now_v7(), "form_id": dd_form_id}]
            }]
        }]
    });

    let (_, saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        &entry_id.to_string(),
        1,
        "complete",
        content,
    )
    .await;
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["pos_id"],
        uu_pos_id.to_string()
    );
    assert_eq!(
        saved["word"]["forms"]["pos"][1]["pos_id"],
        ud_pos_id.to_string()
    );
    assert_eq!(
        saved["word"]["forms"]["pos"][2]["pos_id"],
        dd_pos_id.to_string()
    );
    let stored: Vec<(String, String)> = sqlx::query_as(
        "SELECT spelling_mode, phonetic_mode FROM lexicon.entry_pos WHERE entry_id = $1 ORDER BY sort_order",
    )
    .bind(entry_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        stored,
        [
            ("unified".to_owned(), "unified".to_owned()),
            ("unified".to_owned(), "distinguish".to_owned()),
            ("distinguish".to_owned(), "distinguish".to_owned())
        ]
    );
}

#[sqlx::test]
async fn v3_pronoun_saves_a_fixed_non_base_form_and_round_trips_it(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let surface = format!("v3pronoun{}", admin_id.simple());
    seed_dictionary_word(&pool, &surface).await;

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let entry_id = Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap();
    let mut forms = complete_v3_forms_fixture();
    forms["pos"][0]["pos"] = json!("pronoun");
    forms["pos"][0]["forms"][0]["regional_variants"]["uk"]["spelling"] = json!(surface);
    forms["pos"][0]["forms"][0]["regional_variants"]["us"]["spelling"] =
        json!(format!("{surface}us-base"));
    forms["pos"][0]["forms"][1]["regional_variants"]["uk"]["spelling"] =
        json!(format!("{surface}uk"));
    forms["pos"][0]["forms"][1]["regional_variants"]["us"]["spelling"] =
        json!(format!("{surface}us"));
    let mut comparative = forms["pos"][0]["forms"][0].clone();
    let comparative_id = Uuid::now_v7();
    comparative["id"] = json!(comparative_id);
    comparative["form_type"] = json!("comparative");
    comparative["regional_variants"]["uk"]["id"] = json!(Uuid::now_v7());
    comparative["regional_variants"]["uk"]["spelling"] = json!(format!("more-{surface}"));
    comparative["regional_variants"]["uk"]["pronunciations"][0]["id"] = json!(Uuid::now_v7());
    comparative["regional_variants"]["us"]["id"] = json!(Uuid::now_v7());
    comparative["regional_variants"]["us"]["spelling"] = json!(format!("more-{surface}us"));
    comparative["regional_variants"]["us"]["pronunciations"][0]["id"] = json!(Uuid::now_v7());
    forms["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .push(comparative);
    forms["pos"][0]["form_groups"][0]["members"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": Uuid::now_v7(), "form_id": comparative_id}));

    let (_, saved) =
        save_v3_forms_after_impact(&state, &bearer, &entry_id.to_string(), 1, "complete", forms)
            .await;
    assert_eq!(saved["word"]["forms"]["pos"][0]["pos"], "pronoun");
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["dialect_rules"],
        json!({"spelling_mode": "distinguish", "phonetic_mode": "distinguish"})
    );
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["forms"][2]["form_type"],
        "comparative"
    );
    assert!(
        saved["word"]["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "forms")
    );

    let stored_type: String = sqlx::query_scalar(
        "SELECT form_type FROM lexicon.v3_concrete_forms WHERE entry_id = $1 AND id = $2",
    )
    .bind(entry_id)
    .bind(comparative_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored_type, "comparative");
    let stored_rules: (String, String) = sqlx::query_as(
        "SELECT spelling_mode, phonetic_mode FROM lexicon.entry_pos WHERE entry_id = $1 AND id = $2",
    )
    .bind(entry_id)
    .bind(Uuid::parse_str(saved["word"]["forms"]["pos"][0]["pos_id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        stored_rules,
        ("distinguish".to_owned(), "distinguish".to_owned())
    );

    let (status, reloaded) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reloaded}");
    assert_eq!(
        reloaded["word"]["forms"]["pos"][0]["forms"][2]["id"],
        comparative_id.to_string()
    );
    assert_eq!(
        reloaded["word"]["forms"]["pos"][0]["forms"][2]["form_type"],
        "comparative"
    );
    assert_eq!(
        reloaded["word"]["forms"]["pos"][0]["dialect_rules"],
        saved["word"]["forms"]["pos"][0]["dialect_rules"]
    );
}

#[sqlx::test]
async fn v3_meanings_draft_saves_before_forms_complete_but_complete_is_blocked(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let surface = format!("v3draftmeaning{}", admin_id.simple());
    seed_dictionary_word(&pool, &surface).await;
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let entry_id = created["word"]["id"].as_str().unwrap();
    let pos_id = Uuid::now_v7();
    let (_, forms_saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        entry_id,
        1,
        "save",
        json!({
            "pos": [{
                "pos_id": pos_id,
                "pos": "noun",
                "dialect_rules": {
                    "spelling_mode": "unified",
                    "phonetic_mode": "unified"
                },
                "forms": [],
                "form_groups": []
            }]
        }),
    )
    .await;
    assert!(
        !forms_saved["word"]["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "forms")
    );

    let meanings = json!({
        "sense_groups": [],
        "pos": [{
            "pos_id": pos_id,
            "grammar_structures": [],
            "senses": []
        }]
    });
    let (status, meanings_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 2,
            "intent": "save",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{meanings_saved}");
    assert_eq!(meanings_saved["word"]["revision"], 3);
    assert_eq!(meanings_saved["word"]["max_reachable_step"], "forms");
    assert!(
        !meanings_saved["word"]["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "meanings")
    );

    let (status, _, blocked) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": 3,
            "intent": "complete",
            "content": meanings
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
    assert_eq!(blocked["code"], "step_not_reachable");
    let stored_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(entry_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_revision, 3, "failed complete must not write");
}

/// 语音编辑器在 step 3 写的语法结构标注：三分类不能塌成一类，连读两端的宽度不能丢。
#[sqlx::test]
async fn v3_grammar_annotations_keep_levels_and_liaison_anchors(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let forms_saved = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = forms_saved["word"]["id"].as_str().unwrap();
    let pos_id = forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone();
    let mut meanings = complete_v3_meanings_fixture(pos_id);
    meanings["pos"][0]["grammar_structures"][0]["variants"][0]["content"] = json!({
        "version": 2,
        "text": "countable noun",
        "annotations": [
            {"type": "emphasis", "start": 10, "end": 14, "level": "grammar"},
            // 起点锚点是 "le"、终点锚点是 "n"：两端宽度不同，退化成单字母就会被这条测出来。
            {"type": "liaison", "start": 7, "end": 11, "start_len": 2, "end_len": 1},
            {"type": "emphasis", "start": 0, "end": 9, "level": "function"}
        ]
    });
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": forms_saved["word"]["revision"],
            "intent": "complete",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    // 缺省宽度不上 wire：end_len 是 1，所以不出现。
    let canonical = json!([
        {"type": "emphasis", "start": 0, "end": 9, "level": "function"},
        {"type": "liaison", "start": 7, "end": 11, "start_len": 2},
        {"type": "emphasis", "start": 10, "end": 14, "level": "grammar"}
    ]);
    assert_eq!(
        saved["word"]["meanings"]["pos"][0]["grammar_structures"][0]["variants"][0]["content"]["annotations"],
        canonical,
        "保存响应应原样带回三分类与连读端点宽度"
    );

    let (status, refetched) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refetched}");
    assert_eq!(
        refetched["word"]["meanings"]["pos"][0]["grammar_structures"][0]["variants"][0]["content"]
            ["annotations"],
        canonical,
        "刷新页面读回的也必须是同一份标注"
    );
}

/// 音色 / 语速是「这段文本将来怎么合成」的配置，必须跟着词条落库；
/// V3 → V2 → V3 往返会吞掉它，所以这条一路走到 GET 才算数。
#[sqlx::test]
async fn v3_voice_profiles_persist_on_grammar_and_english_variants(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let word =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["A harbour sentence."])
            .await;

    // 没配过的节点不该带这个键，否则未同步 spec 的前端会把响应判为非法。
    assert!(
        !serde_json::to_string(&word["word"]["meanings"])
            .unwrap()
            .contains("voice_profile"),
        "未配置时 voice_profile 不能出现在响应里"
    );

    let grammar_profile = json!({"voice_ids": ["sonia", "ryan"], "rate_percent": -25});
    let sentence_profile = json!({"voice_ids": ["jenny"], "rate_percent": 0});
    let mut meanings = word["word"]["meanings"].clone();
    meanings["pos"][0]["grammar_structures"][0]["variants"][0]["voice_profile"] =
        grammar_profile.clone();
    meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["voice_profile"] =
        sentence_profile.clone();
    let saved = save_v3_meanings(&state, &bearer, &word, meanings).await;

    let assert_profiles = |body: &Value, label: &str| {
        assert_eq!(
            body["word"]["meanings"]["pos"][0]["grammar_structures"][0]["variants"][0]["voice_profile"],
            grammar_profile,
            "{label}：语法结构变体的音色配置丢了"
        );
        assert_eq!(
            body["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["voice_profile"],
            sentence_profile,
            "{label}：英文例句变体的音色配置丢了"
        );
    };
    assert_profiles(&saved, "保存响应");

    let (status, refetched) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{}", word["word"]["id"].as_str().unwrap()),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refetched}");
    assert_profiles(&refetched, "刷新页面");

    // 发布路径上还有两处 V2 往返，配置同样不能在这里蒸发。
    let (status, published) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_profiles(&published, "发布响应");
}

#[sqlx::test]
async fn v3_voice_profile_rejects_out_of_range_rate_and_oversized_voice_list(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let forms_saved = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = forms_saved["word"]["id"].as_str().unwrap();
    let pos_id = forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone();

    for profile in [
        json!({"voice_ids": ["sonia"], "rate_percent": 101}),
        json!({"voice_ids": ["sonia"], "rate_percent": -51}),
        json!({"voice_ids": ["sonia", "sonia"], "rate_percent": 0}),
        json!({"voice_ids": [""], "rate_percent": 0}),
        json!({"voice_ids": (0..21).map(|index| format!("v{index}")).collect::<Vec<_>>(),
               "rate_percent": 0}),
    ] {
        let mut meanings = complete_v3_meanings_fixture(pos_id.clone());
        meanings["pos"][0]["grammar_structures"][0]["variants"][0]["voice_profile"] =
            profile.clone();
        let (status, _, problem) = call_problem(
            &state,
            Method::PUT,
            &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
            &bearer,
            None,
            json!({
                "schema_version": 3,
                "base_revision": forms_saved["word"]["revision"],
                "intent": "save",
                "content": meanings
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{profile}: {problem}"
        );
        assert!(
            has_issue(&problem, "voice_profile_invalid"),
            "{profile}: {problem}"
        );
    }

    // 已下线的发音人 alias 不做外键式校验：形状合法就存得进去。
    let mut meanings = complete_v3_meanings_fixture(pos_id);
    meanings["pos"][0]["grammar_structures"][0]["variants"][0]["voice_profile"] =
        json!({"voice_ids": ["a-voice-that-no-longer-exists"], "rate_percent": 100});
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": forms_saved["word"]["revision"],
            "intent": "save",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
}

#[sqlx::test]
async fn v3_definition_grammar_is_optional_for_draft_but_required_for_complete_and_validate(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let forms_saved = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = forms_saved["word"]["id"].as_str().unwrap();
    let pos_id = forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone();
    let mut meanings = complete_v3_meanings_fixture(pos_id);
    meanings["pos"][0]["senses"][0]["definitions"][0]
        .as_object_mut()
        .unwrap()
        .remove("grammar_structure_id");

    let (status, draft_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": forms_saved["word"]["revision"],
            "intent": "save",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{draft_saved}");

    let revision = draft_saved["word"]["revision"].as_i64().unwrap();
    let (status, validation) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/validate"),
        &bearer,
        None,
        Some(json!({"schema_version": 3, "base_revision": revision})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{validation}");
    assert_eq!(validation["valid"], false, "{validation}");
    assert!(
        validation["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| {
                issue["node_id"] == meanings["pos"][0]["senses"][0]["definitions"][0]["id"]
                    && issue["field"] == "grammar_structure_id"
                    && issue["code"] == "definition_invalid"
            })
    );

    let (status, _, blocked) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": revision,
            "intent": "complete",
            "content": meanings
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{blocked}");
    assert!(
        blocked["field_issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| {
                issue["field"] == "grammar_structure_id"
                    && issue["code"] == "definition_invalid"
                    && issue["message"] == "请选择语法结构"
            })
    );
}

#[sqlx::test]
async fn v3_complete_forms_require_pos_and_recompute_meanings_completion(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let forms_saved = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = forms_saved["word"]["id"].as_str().unwrap();
    let noun_pos_id = forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone();

    let (status, meanings_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": forms_saved["word"]["revision"],
            "intent": "complete",
            "content": complete_v3_meanings_fixture(noun_pos_id)
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{meanings_saved}");
    assert!(
        meanings_saved["word"]["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "meanings")
    );

    let adjective_pos_id = Uuid::now_v7();
    let adjective_form_id = Uuid::now_v7();
    let mut expanded_forms = meanings_saved["word"]["forms"].clone();
    expanded_forms["pos"].as_array_mut().unwrap().push(json!({
        "pos_id": adjective_pos_id,
        "pos": "adjective",
        "dialect_rules": {
            "spelling_mode": "unified",
            "phonetic_mode": "unified"
        },
        "forms": [{
            "id": adjective_form_id,
            "form_type": "base",
            "regional_variants": {
                "mode": "common",
                "common": {
                    "id": Uuid::now_v7(),
                    "dialect": "common",
                    "spelling": format!("adjectival{}", admin_id.simple()),
                    "origin": "manual",
                    "pronunciations": [{
                        "id": Uuid::now_v7(),
                        "dict_phonetic": "/ədʒ/",
                        "actual_pron": "ədʒ",
                        "style": "normal"
                    }]
                }
            }
        }],
        "form_groups": [{
            "id": Uuid::now_v7(),
            "is_regular": true,
            "members": [{"id": Uuid::now_v7(), "form_id": adjective_form_id}]
        }]
    }));
    let (status, expanded) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": meanings_saved["word"]["revision"],
            "intent": "complete",
            "content": expanded_forms
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{expanded}");
    assert!(
        expanded["word"]["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "forms")
    );
    assert!(
        !expanded["word"]["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "meanings"),
        "新增词性后旧 meanings completion 必须失效：{expanded}"
    );

    let mut all_meanings =
        complete_v3_meanings_fixture(expanded["word"]["forms"]["pos"][0]["pos_id"].clone());
    let mut adjective_meanings = complete_v3_meanings_fixture(json!(adjective_pos_id));
    adjective_meanings["pos"][0]["senses"][0]["sub_pos"] = json!("ADJ");
    all_meanings["sense_groups"].as_array_mut().unwrap().extend(
        adjective_meanings["sense_groups"]
            .as_array()
            .unwrap()
            .iter()
            .cloned(),
    );
    all_meanings["pos"].as_array_mut().unwrap().extend(
        adjective_meanings["pos"]
            .as_array()
            .unwrap()
            .iter()
            .cloned(),
    );
    let (status, meanings_recompleted) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": expanded["word"]["revision"],
            "intent": "complete",
            "content": all_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{meanings_recompleted}");
    assert!(
        meanings_recompleted["word"]["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "meanings")
    );

    let mut reduced_forms = meanings_recompleted["word"]["forms"].clone();
    reduced_forms["pos"].as_array_mut().unwrap().pop();
    let (status, impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": meanings_recompleted["word"]["revision"],
            "content": reduced_forms
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(impact["requires_confirmation"], true, "{impact}");
    let (status, reduced) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": meanings_recompleted["word"]["revision"],
            "intent": "complete",
            "confirmed_impact_token": impact["confirmation_token"],
            "content": reduced_forms
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reduced}");
    assert!(
        !reduced["word"]["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "meanings"),
        "删除词性后 meanings completion 也必须失效：{reduced}"
    );

    let stored_revision = reduced["word"]["revision"].as_i64().unwrap();
    let (status, _, rejected) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": stored_revision,
            "intent": "complete",
            "content": {"pos": []}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert!(has_issue(&rejected, "pos_required"), "{rejected}");
    let persisted_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(entry_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(persisted_revision, stored_revision, "失败请求不得写入");
}

#[sqlx::test]
async fn v3_empty_variant_shells_save_without_surfaces_but_cannot_complete(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let current = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = current["word"]["id"].as_str().unwrap();
    let entry_uuid = Uuid::parse_str(entry_id).unwrap();
    let mut shells = current["word"]["forms"].clone();
    shells["pos"][0]["forms"][0]["regional_variants"]["uk"]["spelling"] = json!("  ");
    shells["pos"][0]["forms"][0]["regional_variants"]["us"]["spelling"] = json!("\n");
    shells["pos"][0]["forms"][1]["regional_variants"]["uk"]["spelling"] = json!("");
    shells["pos"][0]["forms"][1]["regional_variants"]["us"]["spelling"] = json!("\t");

    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": current["word"]["revision"],
            "intent": "save",
            "content": shells
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["spelling"],
        ""
    );
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["us"]["spelling"],
        ""
    );
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["forms"][1]["regional_variants"]["uk"]["spelling"],
        ""
    );
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["forms"][1]["regional_variants"]["us"]["spelling"],
        ""
    );
    let stored_shells: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.v3_form_variants WHERE entry_id = $1 AND spelling = '' AND normalized_spelling = ''",
    )
    .bind(entry_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored_shells, 4);
    let active_surfaces: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.surface_sources WHERE entry_id = $1 AND content_schema_version = 3 AND content_scope = 'draft' AND is_deleted = FALSE",
    )
    .bind(entry_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active_surfaces, 0, "空拼写骨架不得生成 surface");

    let (status, detail) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "空拼写骨架必须仍可按 ID 编辑：{detail}"
    );
    let (status, list) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?page=1&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["words"], json!([]), "空拼写骨架不得进入主列表：{list}");
    assert_eq!(list["page"]["total"], 0, "分页总数必须与主列表一致：{list}");
    let (status, stats) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/stats"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_eq!(stats["total"], 0, "空拼写骨架不得进入统计：{stats}");

    let (status, _, rejected) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": saved["word"]["revision"],
            "intent": "complete",
            "content": saved["word"]["forms"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert!(
        has_issue(&rejected, "variant_spelling_required"),
        "{rejected}"
    );
}

#[sqlx::test]
async fn v3_form_storage_uses_the_authoritative_surface_normalization(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let detection_surface = format!("v3normalize{}", admin_id.simple());
    seed_dictionary_word(&pool, &detection_surface).await;
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": detection_surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let entry_id = Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap();
    let pos_id = Uuid::now_v7();
    let form_id = Uuid::now_v7();
    let variant_id = Uuid::now_v7();
    let group_id = Uuid::now_v7();
    let (_, _saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        &entry_id.to_string(),
        1,
        "complete",
        json!({
            "pos": [{
                "pos_id": pos_id,
                "pos": "noun",
                "dialect_rules": {
                    "spelling_mode": "unified",
                    "phonetic_mode": "unified"
                },
                "forms": [{
                    "id": form_id,
                    "form_type": "base",
                    "regional_variants": {
                        "mode": "common",
                        "common": {
                            "id": variant_id,
                            "dialect": "common",
                            "spelling": "  It\u{2019}s\u{3000}Well\u{2014}Known  ",
                            "origin": "manual",
                            "pronunciations": [{
                                "id": Uuid::now_v7(),
                                "dict_phonetic": "/test/",
                                "actual_pron": "test",
                                "style": "normal"
                            }]
                        }
                    }
                }],
                "form_groups": [{
                    "id": group_id,
                    "is_regular": true,
                    "members": [{"id": Uuid::now_v7(), "form_id": form_id}]
                }]
            }]
        }),
    )
    .await;

    let (spelling, normalized_spelling, normalization_version): (String, String, i16) =
        sqlx::query_as(
            r#"
            SELECT spelling, normalized_spelling, normalization_version
            FROM lexicon.v3_form_variants
            WHERE id = $1 AND entry_id = $2
            "#,
        )
        .bind(variant_id)
        .bind(entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(spelling, "It’s Well—Known");
    assert_eq!(normalized_spelling, "it's well-known");
    assert_eq!(normalization_version, HEADWORD_NORMALIZATION_VERSION);
    let projected: Vec<(String, String, i16)> = sqlx::query_as(
        r#"
        SELECT DISTINCT surface, normalized_surface, normalization_version
        FROM lexicon.surface_sources
        WHERE entry_id = $1
          AND source_node_id = $2
          AND content_schema_version = 3
          AND content_scope = 'draft'
          AND is_deleted = FALSE
        "#,
    )
    .bind(entry_id)
    .bind(variant_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        projected,
        vec![(
            spelling,
            normalized_spelling,
            HEADWORD_NORMALIZATION_VERSION
        )],
        "canonical V3 row and surface projection must share normalization v1"
    );
}

#[sqlx::test]
async fn v3_rejects_conflicting_pending_relation_glosses_with_closed_issue_code(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let forms_saved = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = forms_saved["word"]["id"].as_str().unwrap();
    let pending_headword = format!("vthreeconflict{}", admin_id.simple());
    let mut meanings =
        complete_v3_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone());
    meanings["pos"][0]["senses"][0]["relations"] = json!([
        {
            "id": Uuid::now_v7(),
            "relation": "synonym",
            "pending_target_headword": pending_headword,
            "pending_target_gloss": "V3 第一个预定义词义",
            "score": "82.00"
        },
        {
            "id": Uuid::now_v7(),
            "relation": "antonym",
            "pending_target_headword": pending_headword,
            "pending_target_gloss": "V3 第二个预定义词义",
            "score": "64.00"
        }
    ]);

    let (status, blocked) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": forms_saved["word"]["revision"],
            "intent": "complete",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "V3 应返回结构化 gloss 冲突：{blocked}"
    );
    assert!(
        has_issue(&blocked, "relation_pending_gloss_conflict"),
        "V3 issue code 必须属于闭合集合：{blocked}"
    );
}

#[sqlx::test]
async fn v3_pending_relation_gloss_round_trips_and_materializes(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let forms_saved = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = forms_saved["word"]["id"].as_str().unwrap();
    let pending_headword = format!("vthreepending{}", admin_id.simple());
    let pending_gloss = "V3 预定义中文词义";
    let mut meanings =
        complete_v3_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone());
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "pending_target_headword": pending_headword,
        "pending_target_gloss": pending_gloss,
        "score": "82.00"
    }]);

    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": forms_saved["word"]["revision"],
            "intent": "complete",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "V3 pending gloss 保存失败：{saved}");
    let saved_relation = &saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(saved_relation["pending_target_headword"], pending_headword);
    assert_eq!(saved_relation["pending_target_gloss"], pending_gloss);

    let (status, published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "base_revision": saved["word"]["revision"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "V3 pending gloss 发布失败：{published}"
    );
    let published_relation = &published["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert!(published_relation["target_word_id"].is_string());
    assert!(published_relation["pending_target_headword"].is_null());
    assert!(published_relation["pending_target_gloss"].is_null());

    let (status, reloaded_source) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "发布后读取 V3 源词条失败：{reloaded_source}"
    );
    let reloaded_relation =
        &reloaded_source["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert!(reloaded_relation["target_word_id"].is_string());
    assert!(reloaded_relation["pending_target_headword"].is_null());
    assert!(reloaded_relation["pending_target_gloss"].is_null());

    let materialized_id: Uuid = sqlx::query_scalar(
        "SELECT entry_id FROM lexicon.entry_headword_keys WHERE normalized_headword = $1 LIMIT 1",
    )
    .bind(&pending_headword)
    .fetch_one(&pool)
    .await
    .unwrap();
    let (status, materialized) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{materialized_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "读取 V3 物化目标失败：{materialized}"
    );
    assert_eq!(
        materialized["word"]["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"],
        pending_gloss
    );
}

#[sqlx::test]
async fn v3_draft_relation_prebinding_promotes_once_and_detaches_without_rebinding(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let target_id = target_forms["word"]["id"].as_str().unwrap();
    let target_headword = target_forms["word"]["presentation"]["label"]
        .as_str()
        .unwrap();

    let (status, default_search) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=harbour&kind=word&match_mode=exact&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "默认搜索失败：{default_search}");
    assert!(
        default_search["results"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "未 opt-in 时不得暴露草稿：{default_search}"
    );

    let (status, draft_search) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=harbour&kind=word&match_mode=exact&page_size=20&include_drafts=true"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "草稿搜索失败：{draft_search}");
    let target_result = draft_search["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["entry_id"] == target_id)
        .expect("include_drafts 应返回零词义目标草稿");
    assert_eq!(target_result["status"], "draft");
    assert_eq!(target_result["senses"], json!([]));

    let source_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let source_id = source_forms["word"]["id"].as_str().unwrap();
    let relation_id = Uuid::now_v7();
    let mut source_meanings =
        complete_v3_meanings_fixture(source_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    source_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id,
        "relation": "synonym",
        "prebound_target_word_id": target_id,
        "pending_target_gloss": "管理员预先填写的释义",
        "score": "88.00"
    }]);
    let (status, source_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": source_forms["word"]["revision"],
            "intent": "complete",
            "content": source_meanings.clone()
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "零词义预绑定保存失败：{source_saved}"
    );
    let source_revision = source_saved["word"]["revision"].as_i64().unwrap();
    let waiting = &source_saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(waiting["prebound_target_word_id"], target_id);
    assert_eq!(waiting["prebinding_state"], "waiting_first_sense");
    assert_eq!(waiting["target_status"], "draft");
    assert_eq!(waiting["pending_target_gloss"], "管理员预先填写的释义");
    assert!(
        waiting["pending_target_headword"].is_null(),
        "预绑定不携带待建词面：{waiting}"
    );
    assert_eq!(
        waiting["target_headword"], target_headword,
        "预绑定词面回显走只读 target_headword：{waiting}"
    );

    let disabled_redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let mut disabled_flags = SmartLexiconV3Flags::all_enabled();
    disabled_flags.draft_relation_prebinding = false;
    let disabled_state = AppState::for_test_with_redis(pool.clone(), disabled_redis)
        .with_smart_lexicon_v3_flags_for_test(disabled_flags);
    let disabled_bearer = token(&disabled_state, admin_id);
    let downgraded_relation = source_meanings["pos"][0]["senses"][0]["relations"][0]
        .as_object_mut()
        .unwrap();
    downgraded_relation.remove("prebound_target_word_id");
    // 旧客户端不认识预绑定，会按纯待建形态回发词面。
    downgraded_relation.insert("pending_target_headword".to_owned(), json!(target_headword));
    let (status, disabled_save) = call(
        &disabled_state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_id}/steps/meanings"),
        &disabled_bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": source_revision,
            "intent": "complete",
            "content": source_meanings
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "capability 关闭时旧客户端不得清掉稳定预绑定：{disabled_save}"
    );
    assert_eq!(
        disabled_save["code"],
        "smart_lexicon_v3_storage_unavailable"
    );

    let (status, delete_blocked) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{target_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": target_forms["word"]["revision"],
            "base_lifecycle_revision": target_forms["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "预绑定目标不得永久删除：{delete_blocked}"
    );
    assert_eq!(
        delete_blocked["code"],
        "entry_has_inbound_prebound_relations"
    );

    let (status, waiting_publish) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{source_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "base_revision": source_revision
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "waiting 来源发布必须阻断：{waiting_publish}"
    );
    assert!(has_issue(
        &waiting_publish,
        "relation_prebound_target_has_no_sense"
    ));

    let (status, archived_target) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": target_forms["word"]["revision"],
            "base_lifecycle_revision": target_forms["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "预绑定目标归档失败：{archived_target}"
    );
    let (status, archived_source) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        archived_source["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_status"],
        "archived"
    );
    let mut archived_publish_body = json!({
        "schema_version": 3,
        "base_revision": source_revision
    });
    let (mut status, mut archived_publish) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{source_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(archived_publish_body.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT
        && archived_publish["code"] == "surface_match_acknowledgement_required"
    {
        archived_publish_body["confirmed_surface_match_token"] =
            archived_publish["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
        (status, archived_publish) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/entries/{source_id}/publications"),
            &bearer,
            Some(Uuid::now_v7()),
            Some(archived_publish_body),
        )
        .await;
    }
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "归档目标来源发布必须阻断：{archived_publish}"
    );
    assert!(has_issue(
        &archived_publish,
        "relation_prebound_target_archived"
    ));
    let mut restore_body = json!({
        "base_revision": archived_target["word"]["revision"],
        "base_lifecycle_revision": archived_target["word"]["lifecycle_revision"]
    });
    let (mut status, mut restored_target) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(restore_body.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT
        && restored_target["code"] == "surface_match_acknowledgement_required"
    {
        restore_body["confirmed_surface_match_token"] =
            restored_target["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
        (status, restored_target) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/entries/{target_id}/restore"),
            &bearer,
            Some(Uuid::now_v7()),
            Some(restore_body),
        )
        .await;
    }
    assert_eq!(
        status,
        StatusCode::OK,
        "预绑定目标恢复失败：{restored_target}"
    );

    let mut target_meanings =
        complete_v3_meanings_fixture(target_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    let first_sense_id = target_meanings["pos"][0]["senses"][0]["id"].clone();
    let mut second_sense = target_meanings["pos"][0]["senses"][0].clone();
    second_sense["id"] = json!(Uuid::now_v7());
    second_sense["definitions"][0]["id"] = json!(Uuid::now_v7());
    second_sense["definitions"][0]["content_id"] = json!(Uuid::now_v7());
    second_sense["definitions"][0]["content"]["text"] = json!("第二条，不得优先绑定");
    target_meanings["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .push(second_sense);

    let meanings_events_before: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM platform.outbox_events
        WHERE aggregate_type = 'lexicon.entry'
          AND aggregate_id = $1
          AND event_type = 'lexicon.entry.draft_meanings_saved'
        "#,
    )
    .bind(Uuid::parse_str(target_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();

    let (status, target_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": target_forms["word"]["revision"],
            "intent": "complete",
            "content": target_meanings.clone()
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "目标第一词义保存失败：{target_saved}"
    );
    let meanings_events: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM platform.outbox_events
        WHERE aggregate_type = 'lexicon.entry'
          AND aggregate_id = $1
          AND event_type = 'lexicon.entry.draft_meanings_saved'
        "#,
    )
    .bind(Uuid::parse_str(target_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        meanings_events,
        meanings_events_before + 1,
        "meanings 保存必须新增事件以失效草稿搜索游标"
    );

    let (status, promoted_source) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "转正后读取来源失败：{promoted_source}"
    );
    assert_eq!(promoted_source["word"]["revision"], source_revision + 1);
    let promoted = &promoted_source["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(promoted["id"], relation_id.to_string());
    assert_eq!(promoted["target_word_id"], target_id);
    assert_eq!(promoted["target_sense_id"], first_sense_id);
    assert!(promoted["prebound_target_word_id"].is_null());
    assert!(promoted["pending_target_gloss"].is_null());

    target_meanings["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] =
        json!("港湾（更新文本但保留稳定词义 ID）");
    let (status, target_resaved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": target_saved["word"]["revision"],
            "intent": "complete",
            "content": target_meanings.clone()
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "重复 reconciliation 保存失败：{target_resaved}"
    );
    let (status, source_after_repeat) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(source_after_repeat["word"]["revision"], source_revision + 1);
    assert_eq!(
        source_after_repeat["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_sense_id"],
        first_sense_id
    );

    let mut without_first = target_meanings;
    without_first["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    let (status, target_without_sense) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": target_resaved["word"]["revision"],
            "intent": "save",
            "content": without_first
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "删除已绑定词义失败：{target_without_sense}"
    );

    let (status, detached_source) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "退回后读取来源失败：{detached_source}"
    );
    let detached = &detached_source["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(detached["prebound_target_word_id"], target_id);
    assert_eq!(detached["prebinding_state"], "target_sense_deleted");
    assert!(detached["target_sense_id"].is_null());
    assert!(
        detached["pending_target_headword"].is_null(),
        "退回预绑定后不得回填待建词面：{detached}"
    );
    assert_eq!(
        detached["target_headword"], target_headword,
        "退回预绑定后词面回显保留在只读 target_headword：{detached}"
    );
    let (status, detached_validation) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{source_id}/validate"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": detached_source["word"]["revision"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "detached 校验失败：{detached_validation}"
    );
    assert_eq!(detached_validation["valid"], false);
    assert!(
        detached_validation["issues"]
            .as_array()
            .is_some_and(|issues| issues
                .iter()
                .any(|issue| issue["code"] == "relation_target_sense_deleted")),
        "detached 必须返回稳定 issue，而不是把展示词面当 text pending：{detached_validation}"
    );

    let replacement = complete_v3_meanings_fixture(
        target_without_sense["word"]["forms"]["pos"][0]["pos_id"].clone(),
    );
    let (status, recreated) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": target_without_sense["word"]["revision"],
            "intent": "complete",
            "content": replacement
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "目标重建词义失败：{recreated}");
    let (status, still_detached_source) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let still_detached =
        &still_detached_source["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(still_detached["prebinding_state"], "target_sense_deleted");
    assert!(still_detached["target_sense_id"].is_null());

    let replacement_sense_id = recreated["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();
    let mut repaired_meanings = still_detached_source["word"]["meanings"].clone();
    let repaired_relation = repaired_meanings["pos"][0]["senses"][0]["relations"][0]
        .as_object_mut()
        .unwrap();
    repaired_relation.insert("target_word_id".to_owned(), json!(target_id));
    repaired_relation.insert("target_sense_id".to_owned(), replacement_sense_id.clone());
    for read_or_prebound in [
        "prebound_target_word_id",
        "prebinding_state",
        "target_status",
        "pending_target_headword",
        "pending_target_gloss",
        "target_headword",
        "target_gloss",
    ] {
        repaired_relation.remove(read_or_prebound);
    }
    let (status, repaired_source) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": still_detached_source["word"]["revision"],
            "intent": "complete",
            "content": repaired_meanings
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "detached 显式重选失败：{repaired_source}"
    );
    let repaired = &repaired_source["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(repaired["id"], relation_id.to_string());
    assert_eq!(repaired["target_word_id"], target_id);
    assert_eq!(repaired["target_sense_id"], replacement_sense_id);
    assert!(repaired["prebound_target_word_id"].is_null());
}

#[sqlx::test]
async fn draft_candidates_are_visible_only_to_their_creator(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let owner_id = seed_admin(&pool).await;
    let owner = token(&state, owner_id);
    let outsider_id = seed_admin(&pool).await;
    let outsider = token(&state, outsider_id);

    let owner_forms = create_v3_with_complete_forms(&state, &pool, &owner).await;
    let owner_entry_id = owner_forms["word"]["id"].as_str().unwrap();

    let search_path = format!(
        "{ROOT}/entries/related-search?q=harbour&kind=word&match_mode=exact&page_size=20&include_drafts=true"
    );
    let (status, outsider_search) =
        call(&state, Method::GET, &search_path, &outsider, None, None).await;
    assert_eq!(status, StatusCode::OK, "{outsider_search}");
    assert!(
        outsider_search["results"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "别人的未发布草稿不得进入关联词候选：{outsider_search}"
    );

    let (status, owner_search) = call(&state, Method::GET, &search_path, &owner, None, None).await;
    assert_eq!(status, StatusCode::OK, "{owner_search}");
    let owner_results = owner_search["results"].as_array().unwrap();
    assert_eq!(
        owner_results.len(),
        1,
        "创建者应看到自己的草稿：{owner_search}"
    );
    assert_eq!(owner_results[0]["entry_id"], owner_entry_id);

    // 例句发现的草稿候选走同一条边界：别人的未发布草稿不可见。
    let discovery_body = json!({
        "schema_version": 3,
        "sentence_text": "The harbour is calm.",
        "source_dialect": "common",
        "mode": "selected_segments",
        "selected_segments": [{ "start": 4, "end": 11, "surface": "harbour" }],
        "include_drafts": true,
        "page_size_per_range": 20
    });
    let (status, outsider_discovery) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/sentence-targets/resolve"),
        &outsider,
        None,
        Some(discovery_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outsider_discovery}");
    assert!(
        outsider_discovery["range_results"][0]["draft_matches"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "别人的未发布草稿不得进入发现候选：{outsider_discovery}"
    );

    let (status, owner_discovery) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/sentence-targets/resolve"),
        &owner,
        None,
        Some(discovery_body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{owner_discovery}");
    let owner_matches = owner_discovery["range_results"][0]["draft_matches"]
        .as_array()
        .unwrap();
    assert!(
        owner_matches
            .iter()
            .any(|candidate| candidate["entry_id"] == owner_entry_id),
        "创建者应在发现候选中看到自己的草稿：{owner_discovery}"
    );
}

#[sqlx::test]
async fn surface_machinery_hides_other_admins_drafts(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let owner_id = seed_admin(&pool).await;
    let owner = token(&state, owner_id);
    let outsider_id = seed_admin(&pool).await;
    let outsider = token(&state, outsider_id);

    let owner_forms = create_v3_with_complete_forms(&state, &pool, &owner).await;
    let owner_entry_id = owner_forms["word"]["id"].as_str().unwrap();

    // 检测：别人的未发布草稿不得亮进 surface warning。
    let detect_body = json!({
        "schema_version": 3,
        "language": "en",
        "kind": "word",
        "surface": "harbour"
    });
    let (status, outsider_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &outsider,
        None,
        Some(detect_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outsider_detection}");
    assert_eq!(
        outsider_detection["requires_acknowledgement"], false,
        "别人的草稿不构成检测确认前提：{outsider_detection}"
    );
    assert!(
        outsider_detection["surface_match_page"].is_null(),
        "别人的草稿命中与内容不得进检测页：{outsider_detection}"
    );

    // 正向对照：创建者自己检测必须仍能看到自己的草稿（防空实现全绿）。
    let (status, owner_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &owner,
        None,
        Some(detect_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{owner_detection}");
    assert_eq!(owner_detection["requires_acknowledgement"], true);
    assert!(
        owner_detection["surface_match_page"]["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "创建者应看到自己草稿的命中：{owner_detection}"
    );

    // 建档：同名草稿共存，无需 acknowledge 一个自己看不见的冲突。
    let (status, outsider_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &outsider,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": outsider_detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "撞别人隐形草稿的建档应直接放行：{outsider_created}"
    );
    let outsider_entry_id = outsider_created["word"]["id"].as_str().unwrap();

    // 词形步：impact 预览与保存都不得被别人的草稿词形拦下。
    let forms_content = complete_v3_forms_fixture();
    let (status, impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{outsider_entry_id}/steps/forms/impact"),
        &outsider,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "content": forms_content.clone()
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert!(
        impact["surface_match_page"].is_null(),
        "词形步不得亮出别人的草稿词形：{impact}"
    );
    let mut forms_input = json!({
        "schema_version": 3,
        "base_revision": 1,
        "intent": "complete",
        "content": forms_content
    });
    // 词义连带影响确认是与 surface 无关的既有机制，照常携带。
    if let Some(impact_token) = impact["confirmation_token"].as_str() {
        forms_input["confirmed_impact_token"] = json!(impact_token);
    }
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{outsider_entry_id}/steps/forms"),
        &outsider,
        None,
        Some(forms_input),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "词形保存不得要求确认别人的草稿：{saved}"
    );

    // 过滤是双向的：owner 检测同样看不到 outsider 的草稿。
    let (status, owner_redetection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &owner,
        None,
        Some(detect_body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{owner_redetection}");
    let owner_items = owner_redetection["surface_match_page"]["items"]
        .as_array()
        .expect("创建者应仍能看到自己草稿的命中");
    assert!(
        owner_items
            .iter()
            .all(|item| item["match"]["entry_id"] == owner_entry_id),
        "对方草稿对创建者同样隐形：{owner_redetection}"
    );
}

#[sqlx::test]
async fn publish_ignores_other_admins_drafts_and_coexists_after_ack(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let owner_id = seed_admin(&pool).await;
    let owner = token(&state, owner_id);
    let outsider_id = seed_admin(&pool).await;
    let outsider = token(&state, outsider_id);

    let owner_forms = create_v3_with_complete_forms(&state, &pool, &owner).await;
    let owner_entry_id = owner_forms["word"]["id"].as_str().unwrap();

    // outsider 全链手动走且不带任何 surface token（刻意不用会自动 acknowledge 的
    // create_v3_with_complete_forms——否则词形步证据会覆盖 publish 期重算的集合，
    // 过滤被整体移除时本测试照样绿，失去判别力）。
    let (status, outsider_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &outsider,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "harbour"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outsider_detection}");
    assert!(
        outsider_detection["surface_match_page"].is_null(),
        "{outsider_detection}"
    );
    let (status, outsider_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &outsider,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": outsider_detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{outsider_created}");
    let outsider_entry_id = outsider_created["word"]["id"].as_str().unwrap();
    let forms_content = complete_v3_forms_fixture();
    let (status, impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{outsider_entry_id}/steps/forms/impact"),
        &outsider,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "content": forms_content.clone()
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert!(
        impact["surface_match_page"].is_null(),
        "词形步不得亮出别人的草稿词形：{impact}"
    );
    let mut forms_input = json!({
        "schema_version": 3,
        "base_revision": 1,
        "intent": "complete",
        "content": forms_content
    });
    if let Some(impact_token) = impact["confirmation_token"].as_str() {
        forms_input["confirmed_impact_token"] = json!(impact_token);
    }
    let (status, outsider_forms) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{outsider_entry_id}/steps/forms"),
        &outsider,
        None,
        Some(forms_input),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "词形保存不得要求确认别人的草稿：{outsider_forms}"
    );
    let outsider_meanings =
        complete_v3_meanings_fixture(outsider_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    let (status, outsider_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{outsider_entry_id}/steps/meanings"),
        &outsider,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": outsider_forms["word"]["revision"],
            "intent": "complete",
            "content": outsider_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outsider_saved}");
    let (status, outsider_published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{outsider_entry_id}/publications"),
        &outsider,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "base_revision": outsider_saved["word"]["revision"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "别人的草稿不构成发布约束，发布不得要求 surface 确认：{outsider_published}"
    );

    // owner 随后发布：对 outsider 已发布词面的警告照常（已发布内容全员可见）。
    let owner_meanings =
        complete_v3_meanings_fixture(owner_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    let (status, owner_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{owner_entry_id}/steps/meanings"),
        &owner,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": owner_forms["word"]["revision"],
            "intent": "complete",
            "content": owner_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{owner_saved}");
    let owner_publish_body = json!({
        "schema_version": 3,
        "base_revision": owner_saved["word"]["revision"]
    });
    let publish_key = Uuid::now_v7();
    let (status, owner_warning) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{owner_entry_id}/publications"),
        &owner,
        Some(publish_key),
        Some(owner_publish_body.clone()),
    )
    .await;
    // 对方已发布 → 词面全员可见，发布警告照常；亮出的必须只有已发布内容。
    assert_eq!(status, StatusCode::CONFLICT, "{owner_warning}");
    assert_eq!(
        owner_warning["code"], "surface_match_acknowledgement_required",
        "{owner_warning}"
    );
    // content_scope 是行级判别：status 是词条级 lifecycle，outsider 词条发布后其
    // 工作区 draft 行也会报 published，只有 content_scope 能钉住「无草稿行泄露」。
    assert!(
        owner_warning["meta"]["surface_match_page"]["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()
                && items.iter().all(|item| {
                    item["match"]["status"] == "published"
                        && item["match"]["content_scope"] == "current_publication"
                })),
        "发布警告只得亮出已发布内容：{owner_warning}"
    );
    // V3 不写 entry_headword_keys，同名多 active 发布由 surface policy 治理：
    // acknowledge 已发布词面后照常共存，过滤不改变这条既有语义。
    let mut confirmed_publish = owner_publish_body;
    confirmed_publish["confirmed_surface_match_token"] =
        owner_warning["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
    let (status, owner_publish) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{owner_entry_id}/publications"),
        &owner,
        Some(publish_key),
        Some(confirmed_publish),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{owner_publish}");
}

#[sqlx::test]
async fn inbound_relation_previews_hide_other_admins_draft_sources(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let publisher_id = seed_admin(&pool).await;
    let publisher = token(&state, publisher_id);
    let referrer_id = seed_admin(&pool).await;
    let referrer = token(&state, referrer_id);
    let outsider_id = seed_admin(&pool).await;
    let outsider = token(&state, outsider_id);

    // publisher 发布目标词条 P（harbour）。
    let target_forms = create_v3_with_complete_forms(&state, &pool, &publisher).await;
    let target_id = target_forms["word"]["id"].as_str().unwrap();
    let target_meanings =
        complete_v3_meanings_fixture(target_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    let (status, target_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_id}/steps/meanings"),
        &publisher,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": target_forms["word"]["revision"],
            "intent": "complete",
            "content": target_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{target_saved}");
    let (status, target_published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/publications"),
        &publisher,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "base_revision": target_saved["word"]["revision"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{target_published}");
    let target_sense_id = target_published["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();

    // referrer 的未发布草稿引用 P。
    let (status, referrer_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &referrer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "dockyard"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{referrer_detection}");
    let (status, referrer_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &referrer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": referrer_detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{referrer_created}");
    let referrer_entry_id = referrer_created["word"]["id"].as_str().unwrap();
    let (_, referrer_forms) = save_v3_forms_after_impact(
        &state,
        &referrer,
        referrer_entry_id,
        1,
        "complete",
        complete_v3_forms_fixture(),
    )
    .await;
    let mut referrer_meanings =
        complete_v3_meanings_fixture(referrer_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    referrer_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": target_id,
        "target_sense_id": target_sense_id,
        "score": "88.00"
    }]);
    let (status, referrer_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{referrer_entry_id}/steps/meanings"),
        &referrer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": referrer_forms["word"]["revision"],
            "intent": "complete",
            "content": referrer_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{referrer_saved}");

    let detect_body = json!({
        "schema_version": 3,
        "language": "en",
        "kind": "word",
        "surface": "harbour"
    });
    let context_for = |detection: &Value, entry_id: &str| -> Value {
        detection["surface_match_page"]["matched_entry_contexts"]
            .as_array()
            .unwrap_or_else(|| panic!("检测应携带命中上下文：{detection}"))
            .iter()
            .find(|context| context["entry_id"] == entry_id)
            .unwrap_or_else(|| panic!("检测应命中目标词条：{detection}"))
            .clone()
    };

    // 外人检测命中 P：入站预览不得亮出 referrer 的未发布草稿。
    let (status, outsider_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &outsider,
        None,
        Some(detect_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outsider_detection}");
    let outsider_context = context_for(&outsider_detection, target_id);
    assert_eq!(
        outsider_context["inbound_relations"]["total"], 0,
        "别人的草稿引用不得进入站预览：{outsider_context}"
    );

    // 创建者自己检测：能看到自己草稿的引用。
    let (status, referrer_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &referrer,
        None,
        Some(detect_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{referrer_detection}");
    let referrer_context = context_for(&referrer_detection, target_id);
    assert!(
        referrer_context["inbound_relations"]["previews"]
            .as_array()
            .is_some_and(|previews| previews.iter().any(|preview| {
                preview["source_entry_id"] == referrer_entry_id
                    && preview["source_status"] == "draft"
            })),
        "创建者应看到自己草稿的引用：{referrer_context}"
    );

    // referrer 发布引用词条后：外人经发布分支看到引用（钉死去重条件不双失明），
    // 创建者经草稿分支看到，两边各恰一条。
    let (status, referrer_published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{referrer_entry_id}/publications"),
        &referrer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "base_revision": referrer_saved["word"]["revision"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{referrer_published}");
    for (label, bearer) in [("外人", &outsider), ("创建者", &referrer)] {
        let (status, detection) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/detections"),
            bearer,
            None,
            Some(detect_body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{detection}");
        let context = context_for(&detection, target_id);
        let previews = context["inbound_relations"]["previews"]
            .as_array()
            .unwrap_or_else(|| panic!("发布后的引用对{label}应可见：{context}"))
            .iter()
            .filter(|preview| preview["source_entry_id"] == referrer_entry_id)
            .count();
        assert_eq!(previews, 1, "{label}应恰好看到一条已发布引用：{context}");
    }
}

#[sqlx::test]
async fn prebound_gloss_is_field_validated_on_save(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let target_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let target_id = target_forms["word"]["id"].as_str().unwrap();
    let source_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let source_id = source_forms["word"]["id"].as_str().unwrap();
    let mut source_meanings =
        complete_v3_meanings_fixture(source_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    source_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "prebound_target_word_id": target_id,
        "pending_target_gloss": "超".repeat(5001),
        "score": "10"
    }]);
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": source_forms["word"]["revision"],
            "intent": "save",
            "content": source_meanings
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "预绑定超长 gloss 必须是字段级 422 而非撞库约束：{saved}"
    );
    assert!(
        has_issue(&saved, "relation_pending_gloss_invalid"),
        "{saved}"
    );
}

#[sqlx::test]
async fn stale_wide_prebound_projection_heals_on_read(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let target_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let target_id = target_forms["word"]["id"].as_str().unwrap();
    let target_headword = target_forms["word"]["presentation"]["label"]
        .as_str()
        .unwrap();
    let source_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let source_id = source_forms["word"]["id"].as_str().unwrap();
    let mut source_meanings =
        complete_v3_meanings_fixture(source_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    source_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "prebound_target_word_id": target_id,
        "score": "10"
    }]);
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": source_forms["word"]["revision"],
            "intent": "save",
            "content": source_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    // 把投影 JSONB 手术回迁移前的旧宽形态：带待建词面、缺只读词面回显。
    sqlx::query(
        r#"
        UPDATE lexicon.entry_editor_projection
        SET meanings = jsonb_set(
            meanings,
            '{pos,0,senses,0,relations,0}',
            (meanings #> '{pos,0,senses,0,relations,0}') - 'target_headword'
                || jsonb_build_object('pending_target_headword', $2::text)
        )
        WHERE entry_id = $1
        "#,
    )
    .bind(Uuid::parse_str(source_id).unwrap())
    .bind(target_headword)
    .execute(&pool)
    .await
    .unwrap();

    let (status, reread) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reread}");
    let relation = &reread["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert!(
        relation["pending_target_headword"].is_null(),
        "读路径必须剥掉旧宽形态的待建词面：{relation}"
    );
    assert_eq!(
        relation["target_headword"], target_headword,
        "读路径必须按目标当前 presentation 回填词面回显：{relation}"
    );
}

#[sqlx::test]
async fn v3_relation_prebinding_reconciliation_is_atomic_at_500_and_501(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    for relation_count in [500usize, 501usize] {
        let target_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
        let target_id = target_forms["word"]["id"].as_str().unwrap();
        let source_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
        let source_id = source_forms["word"]["id"].as_str().unwrap();
        let mut source_meanings =
            complete_v3_meanings_fixture(source_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
        source_meanings["pos"][0]["senses"][0]["relations"] = Value::Array(
            (0..relation_count)
                .map(|ordinal| {
                    json!({
                        "id": Uuid::now_v7(),
                        "relation": "synonym",
                        "prebound_target_word_id": target_id,
                        "score": format!("{}", ordinal % 101)
                    })
                })
                .collect(),
        );
        let (status, source_saved) = call(
            &state,
            Method::PUT,
            &format!("{ROOT}/entries/{source_id}/steps/meanings"),
            &bearer,
            None,
            Some(json!({
                "schema_version": 3,
                "base_revision": source_forms["word"]["revision"],
                "intent": "complete",
                "content": source_meanings
            })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{relation_count} 条预绑定保存失败：{source_saved}"
        );
        let source_revision = source_saved["word"]["revision"].as_i64().unwrap();
        let target_revision = target_forms["word"]["revision"].as_i64().unwrap();
        let target_meanings =
            complete_v3_meanings_fixture(target_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
        let (status, target_saved) = call(
            &state,
            Method::PUT,
            &format!("{ROOT}/entries/{target_id}/steps/meanings"),
            &bearer,
            None,
            Some(json!({
                "schema_version": 3,
                "base_revision": target_revision,
                "intent": "complete",
                "content": target_meanings
            })),
        )
        .await;

        let (read_status, source_after) = call(
            &state,
            Method::GET,
            &format!("{ROOT}/entries/{source_id}"),
            &bearer,
            None,
            None,
        )
        .await;
        assert_eq!(read_status, StatusCode::OK);
        let relations = source_after["word"]["meanings"]["pos"][0]["senses"][0]["relations"]
            .as_array()
            .unwrap();
        assert_eq!(relations.len(), relation_count);
        if relation_count == 500 {
            assert_eq!(status, StatusCode::OK, "500 条应全部成功：{target_saved}");
            assert_eq!(source_after["word"]["revision"], source_revision + 1);
            assert!(relations.iter().all(|relation| {
                relation["target_word_id"] == target_id
                    && relation["prebound_target_word_id"].is_null()
            }));
        } else {
            assert_eq!(
                status,
                StatusCode::CONFLICT,
                "501 条必须整体拒绝：{target_saved}"
            );
            assert_eq!(target_saved["code"], "relation_prebinding_fanout_exceeded");
            assert_eq!(source_after["word"]["revision"], source_revision);
            assert!(relations.iter().all(|relation| {
                relation["prebound_target_word_id"] == target_id
                    && relation["target_word_id"].is_null()
            }));
            let (target_read_status, target_after) = call(
                &state,
                Method::GET,
                &format!("{ROOT}/entries/{target_id}"),
                &bearer,
                None,
                None,
            )
            .await;
            assert_eq!(target_read_status, StatusCode::OK);
            assert_eq!(target_after["word"]["revision"], target_revision);
            assert!(
                target_after["word"]["meanings"]["pos"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[sqlx::test]
async fn v3_relation_prebinding_uses_nowait_and_retries_without_partial_writes(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let target_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let source_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let target_id = target_forms["word"]["id"].as_str().unwrap();
    let source_id = source_forms["word"]["id"].as_str().unwrap();
    let mut source_meanings =
        complete_v3_meanings_fixture(source_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    source_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "prebound_target_word_id": target_id,
        "score": "80"
    }]);
    let (status, source_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": source_forms["word"]["revision"],
            "intent": "complete",
            "content": source_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{source_saved}");
    let source_revision = source_saved["word"]["revision"].as_i64().unwrap();
    let target_revision = target_forms["word"]["revision"].as_i64().unwrap();
    let target_meanings =
        complete_v3_meanings_fixture(target_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    let target_request = json!({
        "schema_version": 3,
        "base_revision": target_revision,
        "intent": "complete",
        "content": target_meanings
    });

    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM lexicon.entries WHERE id = $1 FOR UPDATE")
        .bind(Uuid::parse_str(source_id).unwrap())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let (status, conflict) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_id}/steps/meanings"),
        &bearer,
        None,
        Some(target_request.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "NOWAIT 应快速返回 409：{conflict}"
    );
    assert_eq!(conflict["code"], "reference_conflict");
    blocker.rollback().await.unwrap();

    let (status, target_after_conflict) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{target_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(target_after_conflict["word"]["revision"], target_revision);
    let (status, source_after_conflict) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(source_after_conflict["word"]["revision"], source_revision);
    assert_eq!(
        source_after_conflict["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["prebinding_state"],
        "waiting_first_sense"
    );

    let (status, retried) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_id}/steps/meanings"),
        &bearer,
        None,
        Some(target_request),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "释放锁后重试应收敛：{retried}");
    let (status, source_after_retry) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(source_after_retry["word"]["revision"], source_revision + 1);
    assert_eq!(
        source_after_retry["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_word_id"],
        target_id
    );
}

#[sqlx::test]
async fn v3_real_http_create_edit_read_validate_and_native_publish(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    seed_dictionary_word(&pool, "harbour").await;

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "harbour"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    assert_eq!(detection["schema_version"], 3);
    assert_eq!(detection["normalized_surface"], "harbour");
    assert_eq!(detection["requires_acknowledgement"], false);

    let create_body = json!({
        "schema_version": 3,
        "detection_id": detection["detection_id"],
        "kind": "word"
    });
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
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["word"]["schema_version"], 3);
    assert_eq!(created["word"]["revision"], 1);
    assert_eq!(
        created["word"]["capabilities"]["publication"],
        json!({"mode": "native"})
    );
    assert!(created["word"].get("headwords").is_none());
    assert!(created["word"].get("compatibility").is_none());
    let entry_id = created["word"]["id"].as_str().unwrap();
    let entry_uuid = Uuid::parse_str(entry_id).unwrap();

    let (status, replayed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(create_key),
        Some(create_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replayed}");
    assert_eq!(replayed["word"]["id"], entry_id);

    let mut conflicting_create = create_body;
    conflicting_create["detection_id"] = json!(Uuid::now_v7());
    let (status, _, conflict) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(create_key),
        conflicting_create,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["code"], "idempotency_conflict");

    let forms = complete_v3_forms_fixture();
    let (impact, saved) =
        save_v3_forms_after_impact(&state, &bearer, entry_id, 1, "complete", forms).await;
    assert_eq!(impact["schema_version"], 3);
    assert_eq!(impact["requires_confirmation"], true);
    assert!(!impact["affected"].as_array().unwrap().is_empty());
    assert_eq!(saved["word"]["revision"], 2);
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["forms"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["form_groups"][1]["members"][0]["form_id"],
        saved["word"]["forms"]["pos"][0]["forms"][0]["id"]
    );
    assert_eq!(
        saved["word"]["presentation"]["matched_surfaces"],
        json!(["harbour", "harbor"])
    );

    let stale_forms = saved["word"]["forms"].clone();
    let (status, _, stale) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "save",
            "content": stale_forms
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["code"], "revision_conflict");

    let (status, meanings_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 2,
            "intent": "complete",
            "content": complete_v3_meanings_fixture(
                saved["word"]["forms"]["pos"][0]["pos_id"].clone()
            )
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{meanings_saved}");
    assert_eq!(meanings_saved["word"]["revision"], 3);

    let (status, validation) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/validate"),
        &bearer,
        None,
        Some(json!({"schema_version": 3, "base_revision": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{validation}");
    assert_eq!(validation["valid"], true);

    let (status, fetched) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fetched}");
    assert_eq!(fetched["word"]["schema_version"], 3);
    assert_eq!(fetched["word"]["revision"], 3);

    let (status, list) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?page=1&page_size=20&q=harbour"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["words"].as_array().unwrap().len(), 1);
    assert_eq!(list["words"][0]["schema_version"], 3);
    assert_eq!(
        list["words"][0]["presentation"],
        saved["word"]["presentation"]
    );
    assert_eq!(
        list["words"][0]["dialects"],
        json!(["uk", "us"]),
        "noun 词性 distinguish → 列表方言摘要为英美：{list}"
    );

    let read_disabled_state = state
        .clone()
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::default());
    let (status, v2_only_list) = call(
        &read_disabled_state,
        Method::GET,
        &format!("{ROOT}/entries?page=1&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v2_only_list}");
    assert!(v2_only_list["words"].as_array().unwrap().is_empty());

    let headword_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_headwords WHERE entry_id = $1")
            .bind(entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(headword_rows, 0, "native V3 不得伪造 legacy headword");
    let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
          (SELECT count(*) FROM lexicon.v3_form_groups WHERE entry_id = $1),
          (SELECT count(*) FROM lexicon.v3_concrete_forms WHERE entry_id = $1),
          (SELECT count(*) FROM lexicon.v3_group_memberships WHERE entry_id = $1),
          (SELECT count(*) FROM lexicon.v3_form_variants WHERE entry_id = $1),
          (SELECT count(*) FROM lexicon.v3_pronunciations WHERE entry_id = $1)
        "#,
    )
    .bind(entry_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (2, 2, 3, 4, 4));
    let surface_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.surface_sources WHERE entry_id = $1 AND content_schema_version = 3 AND is_deleted = FALSE",
    )
    .bind(entry_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        surface_rows, 4,
        "两个 concrete forms 各有四条当前有效的显式 uk/us surface"
    );
    let retired_create_surfaces: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.surface_sources WHERE entry_id = $1 AND content_schema_version = 3 AND is_deleted = TRUE",
    )
    .bind(entry_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        retired_create_surfaces, 2,
        "create-time dictionary suggestion surfaces must remain auditable tombstones after replacement"
    );

    let publish_key = Uuid::now_v7();
    let publish_body = json!({"schema_version": 3, "base_revision": 3});
    let (status, published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(publish_key),
        Some(publish_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_eq!(published["word"]["schema_version"], 3);
    assert_eq!(published["word"]["status"], "published");
    assert_eq!(published["word"]["published_revision"], 3);
    assert!(published["word"].get("compatibility").is_none());

    let (publication_count, publication_schema, current_publication_id): (i64, i16, Uuid) =
        sqlx::query_as(
            r#"
            SELECT
              (SELECT count(*) FROM lexicon.entry_publications WHERE entry_id = entry.id),
              publication.content_schema_version,
              entry.current_publication_id
            FROM lexicon.entries entry
            JOIN lexicon.entry_publications publication
              ON publication.id = entry.current_publication_id
            WHERE entry.id = $1
            "#,
        )
        .bind(entry_uuid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(publication_count, 1);
    assert_eq!(publication_schema, 3);
    assert_eq!(
        published["word"]["published_revision"],
        published["word"]["revision"]
    );

    let current_surface_rows: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM lexicon.surface_sources
        WHERE entry_id = $1
          AND content_scope = 'current_publication'
          AND publication_id = $2
          AND content_schema_version = 3
          AND is_deleted = FALSE
        "#,
    )
    .bind(entry_uuid)
    .bind(current_publication_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(current_surface_rows, 4);

    let (status, replayed_publish) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(publish_key),
        Some(publish_body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replayed_publish}");
    assert_eq!(
        replayed_publish["word"]["published_revision"],
        published["word"]["published_revision"]
    );
    let publication_count_after_replay: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publications WHERE entry_id = $1")
            .bind(entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(publication_count_after_replay, 1);

    let (status, edited_again) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": published["word"]["revision"],
            "intent": "complete",
            "content": meanings_saved["word"]["meanings"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "创建第二版 native 草稿失败：{edited_again}"
    );
    let second_publish_body = json!({
        "schema_version": 3,
        "base_revision": edited_again["word"]["revision"]
    });
    let (status, second_published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(second_publish_body),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "第二版 native publication 发布失败：{second_published}"
    );
    let publications: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM lexicon.entry_publications WHERE entry_id = $1 ORDER BY publication_number",
    )
    .bind(entry_uuid)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(publications.len(), 2);
    assert_eq!(publications[0], current_publication_id);
    let second_publication_id = publications[1];

    let (status, history) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert_eq!(history["publications"].as_array().unwrap().len(), 2);
    assert_eq!(history["publications"][0]["schema_version"], 3);
    assert_eq!(history["publications"][0]["is_current"], true);

    let (status, published_list) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?page=1&page_size=20&q=harbour"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published_list}");
    assert_eq!(published_list["words"][0]["status"], "published");
    let (status, stats) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/stats"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_eq!(stats["total"], 1);
    let (status, related) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=harbour&kind=word&match_mode=exact&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{related}");
    assert_eq!(related["results"][0]["entry_id"], entry_id, "{related}");

    let mut lifecycle_revision = second_published["word"]["lifecycle_revision"]
        .as_i64()
        .unwrap();
    for publication_id in [current_publication_id, second_publication_id] {
        let (status, activated) = activate_v3_history(
            &state,
            &bearer,
            entry_uuid,
            publication_id,
            second_published["word"]["revision"].as_i64().unwrap(),
            lifecycle_revision,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "native A→B→A 激活失败：{activated}");
        lifecycle_revision += 1;
        assert_eq!(activated["word"]["lifecycle_revision"], lifecycle_revision);
        let current: Uuid =
            sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
                .bind(entry_uuid)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(current, publication_id);
        assert_eq!(
            live_surface_publication_ids(&pool, entry_uuid).await,
            vec![publication_id]
        );
    }

    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": second_published["word"]["revision"],
            "base_lifecycle_revision": lifecycle_revision
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    assert_eq!(archived["word"]["status"], "archived");
    lifecycle_revision += 1;
    let (status, restored) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": second_published["word"]["revision"],
            "base_lifecycle_revision": lifecycle_revision
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["word"]["status"], "published");
    assert_eq!(
        restored["word"]["lifecycle_revision"],
        lifecycle_revision + 1
    );
    let current_after_restore: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current_after_restore, second_publication_id);
    assert_eq!(
        live_surface_publication_ids(&pool, entry_uuid).await,
        vec![second_publication_id]
    );
}

#[sqlx::test]
async fn v3_projection_flag_blocks_every_projection_dependent_write_without_mutation(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let enabled = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&enabled, admin_id);
    let word = create_v3_with_complete_forms(&enabled, &pool, &bearer).await;
    let entry_id = word["word"]["id"].as_str().unwrap();
    let revision = word["word"]["revision"].as_i64().unwrap();
    let projection_disabled =
        enabled
            .clone()
            .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags {
                projection: false,
                ..SmartLexiconV3Flags::all_enabled()
            });

    for (method, path, idempotency_key, body) in [
        (
            Method::PUT,
            format!("{ROOT}/entries/{entry_id}/steps/forms"),
            None,
            json!({
                "schema_version": 3,
                "base_revision": revision,
                "intent": "complete",
                "content": word["word"]["forms"]
            }),
        ),
        (
            Method::PUT,
            format!("{ROOT}/entries/{entry_id}/steps/meanings"),
            None,
            json!({
                "schema_version": 3,
                "base_revision": revision,
                "intent": "complete",
                "content": complete_v3_meanings_fixture(
                    word["word"]["forms"]["pos"][0]["pos_id"].clone()
                )
            }),
        ),
        (
            Method::POST,
            format!("{ROOT}/entries/{entry_id}/publications"),
            Some(Uuid::now_v7()),
            json!({"schema_version": 3, "base_revision": revision}),
        ),
    ] {
        let (status, _, problem) = call_problem(
            &projection_disabled,
            method,
            &path,
            &bearer,
            idempotency_key,
            body,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{problem}");
        assert_eq!(problem["code"], "smart_lexicon_v3_storage_unavailable");
    }

    let stored_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(entry_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_revision, revision);

    let (status, detection) = call(
        &enabled,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "harbour"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let mut create_body = json!({
        "schema_version": 3,
        "detection_id": detection["detection_id"],
        "kind": "word"
    });
    if let Some(token) = detection["surface_match_page"]["surface_confirmation_token"].as_str() {
        create_body["confirmed_surface_match_token"] = json!(token);
    }
    let entries_before: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    let (status, _, problem) = call_problem(
        &projection_disabled,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        create_body,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{problem}");
    let entries_after: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(entries_after, entries_before);
}

#[sqlx::test]
async fn v3_forms_projection_retains_tombstones_and_emits_one_replay_event(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let word = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = Uuid::parse_str(word["word"]["id"].as_str().unwrap()).unwrap();
    let mut proposed = word["word"]["forms"].clone();
    let removed_variant_ids = vec![
        Uuid::parse_str(
            proposed["pos"][0]["forms"][1]["regional_variants"]["uk"]["id"]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
        Uuid::parse_str(
            proposed["pos"][0]["forms"][1]["regional_variants"]["us"]["id"]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
    ];
    proposed["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    proposed["pos"][0]["form_groups"][0]["members"]
        .as_array_mut()
        .unwrap()
        .remove(1);

    let (status, impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": word["word"]["revision"],
            "content": proposed
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(impact["requires_confirmation"], true);
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": word["word"]["revision"],
            "intent": "complete",
            "confirmed_impact_token": impact["confirmation_token"],
            "content": proposed
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["word"]["revision"], 3);

    let (active_removed, retired_removed): (i64, i64) = sqlx::query_as(
        r#"
        SELECT count(*) FILTER (WHERE is_deleted = FALSE),
               count(*) FILTER (WHERE is_deleted = TRUE)
        FROM lexicon.surface_sources
        WHERE entry_id = $1 AND source_node_id = ANY($2)
        "#,
    )
    .bind(entry_id)
    .bind(&removed_variant_ids)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((active_removed, retired_removed), (0, 2));
    let active_projection: (i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT count(*), count(DISTINCT event_offset), min(source_revision)
        FROM lexicon.surface_sources
        WHERE entry_id = $1
          AND content_schema_version = 3
          AND content_scope = 'draft'
          AND is_deleted = FALSE
        "#,
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active_projection, (2, 1, 3));
    let event: Value = sqlx::query_scalar(
        r#"
        SELECT payload
        FROM platform.outbox_events
        WHERE aggregate_type = 'lexicon.surface_projection'
          AND aggregate_id = $1
          AND event_type = 'lexicon.surface_projection_replaced'
        ORDER BY occurred_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(event["content_schema_version"], 3);
    assert_eq!(event["source_revision"], 3);
    assert_eq!(event["source_count"], 2);
}

#[sqlx::test]
async fn v3_forms_impact_canonicalizes_before_issuing_downstream_token(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let word = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = word["word"]["id"].as_str().unwrap();
    let base_revision = word["word"]["revision"].clone();
    let mut proposed = word["word"]["forms"].clone();

    proposed["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    proposed["pos"][0]["form_groups"][0]["members"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    proposed["pos"][0]["forms"][0]["regional_variants"]["uk"]["spelling"] =
        json!("  Ｃａｎｏｎｉｃａｌ　Ｐｒｅｖｉｅｗ  ");

    let (status, impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": base_revision,
            "content": proposed
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(impact["requires_confirmation"], true);
    assert!(impact.get("surface_match_page").is_none(), "{impact}");
    assert!(impact["confirmation_token"].is_string(), "{impact}");

    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": base_revision,
            "intent": "complete",
            "confirmed_impact_token": impact["confirmation_token"],
            "content": proposed
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["spelling"],
        "Canonical Preview"
    );
}

#[sqlx::test]
async fn v3_forms_impact_matches_every_meaning_node_actually_removed_by_save(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let target_entry_id = target_forms["word"]["id"].clone();
    let target_pos_id = target_forms["word"]["forms"]["pos"][0]["pos_id"].clone();
    let target_meanings = complete_v3_meanings_fixture(target_pos_id);
    let target_sense_id = target_meanings["pos"][0]["senses"][0]["id"].clone();
    let (status, target_saved) = call(
        &state,
        Method::PUT,
        &format!(
            "{ROOT}/entries/{}/steps/meanings",
            target_entry_id.as_str().unwrap()
        ),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": target_forms["word"]["revision"],
            "intent": "complete",
            "content": target_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{target_saved}");

    let source_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let source_entry_id = source_forms["word"]["id"].as_str().unwrap().to_owned();
    let source_entry_uuid = Uuid::parse_str(&source_entry_id).unwrap();
    let source_pos_id = source_forms["word"]["forms"]["pos"][0]["pos_id"].clone();
    let mut source_meanings = complete_v3_meanings_fixture(source_pos_id);
    let source_sense_id = source_meanings["pos"][0]["senses"][0]["id"].clone();
    source_meanings["pos"][0]["senses"][0]["sentences"] = json!([{
        "id": Uuid::now_v7(),
        "level": "A1",
        "en_text": {
            "mode": "unified",
            "common": {
                "id": Uuid::now_v7(),
                "value": rich_text("It is a harbour."),
                "origin": "manual"
            }
        },
        "zh_text_id": Uuid::now_v7(),
        "zh_text": rich_text("这是一个港口。"),
        "links": [{
            "word_id": source_entry_uuid,
            "sense_id": source_sense_id,
            "role": "focus"
        }]
    }]);
    source_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": target_entry_id,
        "target_sense_id": target_sense_id,
        "score": "95.00"
    }]);
    let (status, source_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": source_forms["word"]["revision"],
            "intent": "complete",
            "content": source_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{source_saved}");

    let before: Vec<(Uuid, String)> = sqlx::query_as(
        r#"
        SELECT id, node_type
        FROM lexicon.nodes
        WHERE entry_id = $1
          AND removed_from_draft_at IS NULL
          AND node_type = ANY($2)
        "#,
    )
    .bind(source_entry_uuid)
    .bind([
        "pos",
        "form_group",
        "group_membership",
        "concrete_form",
        "form_variant",
        "pronunciation",
        "sense_group",
        "grammar_structure",
        "text_variant",
        "sense",
        "definition",
        "sentence",
        "relation",
    ])
    .fetch_all(&pool)
    .await
    .unwrap();
    let sense_group_id = Uuid::parse_str(
        source_saved["word"]["meanings"]["sense_groups"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let proposed = json!({"pos": []});
    let (status, impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{source_entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": source_saved["word"]["revision"],
            "content": proposed
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(impact["requires_confirmation"], true);

    let reported = impact["affected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                Uuid::parse_str(item["node_id"].as_str().unwrap()).unwrap(),
                item["node_type"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<HashSet<_>>();
    for required_type in [
        "grammar_structure",
        "text_variant",
        "sense",
        "definition",
        "sentence",
        "relation",
    ] {
        assert!(
            reported
                .iter()
                .any(|(_, node_type)| node_type == required_type),
            "impact must expose removed {required_type}: {impact}"
        );
    }

    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": source_saved["word"]["revision"],
            "intent": "save",
            "confirmed_impact_token": impact["confirmation_token"],
            "content": proposed
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["word"]["meanings"]["pos"], json!([]));

    let active_after = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM lexicon.nodes WHERE entry_id = $1 AND removed_from_draft_at IS NULL",
    )
    .bind(source_entry_uuid)
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .collect::<HashSet<_>>();
    let actually_removed = before
        .into_iter()
        .filter(|(id, _)| !active_after.contains(id))
        .map(|(id, node_type)| {
            let impact_type = match node_type.as_str() {
                "group_membership" => "membership",
                "concrete_form" => "form",
                "form_variant" => "variant",
                other => other,
            };
            (id, impact_type.to_owned())
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        reported, actually_removed,
        "impact must equal the exact node set retired by the confirmed save"
    );
    assert!(active_after.contains(&sense_group_id));
    assert!(
        !reported.iter().any(|(id, _)| *id == sense_group_id),
        "top-level sense groups are retained and must not be reported as deleted"
    );
}

#[sqlx::test]
async fn v3_meanings_reject_read_only_and_invalid_complete_without_writing(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let saved = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = saved["word"]["id"].as_str().unwrap();
    let entry_uuid = Uuid::parse_str(entry_id).unwrap();
    let pos_id = saved["word"]["forms"]["pos"][0]["pos_id"].clone();

    let mut sentence_content = complete_v3_meanings_fixture(pos_id.clone());
    let sense_id = sentence_content["pos"][0]["senses"][0]["id"].clone();
    let sentence = json!({
        "id": Uuid::now_v7(),
        "level": "A1",
        "en_text": {
            "mode": "unified",
            "common": {
                "id": Uuid::now_v7(),
                "value": rich_text("It is a harbour."),
                "origin": "manual"
            }
        },
        "zh_text_id": Uuid::now_v7(),
        "zh_text": rich_text("这是一个港口。"),
        "links": [{
            "word_id": entry_uuid,
            "sense_id": sense_id,
            "role": "focus"
        }]
    });
    let mut forged_sentence = sentence.clone();
    forged_sentence["associations"] = json!([]);
    forged_sentence["associations_state"] = json!("unresolved");
    sentence_content["pos"][0]["senses"][0]["sentences"] = json!([forged_sentence]);
    let (status, _, problem) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": 2,
            "intent": "complete",
            "content": sentence_content
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(has_issue(&problem, "forbidden_v3_field"), "{problem}");

    let mut forged = complete_v3_meanings_fixture(pos_id.clone());
    forged["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "pending_target_headword": "port",
        "target_headword": "forged",
        "target_gloss": "forged",
        "score": "50"
    }]);
    let (status, _, problem) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": 2,
            "intent": "complete",
            "content": forged
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(has_issue(&problem, "forbidden_v3_field"), "{problem}");
    assert!(
        problem["field_issues"]
            .as_array()
            .unwrap()
            .iter()
            .all(|issue| issue["schema_version"] == 3)
    );

    let mut invalid = complete_v3_meanings_fixture(pos_id.clone());
    invalid["pos"][0]["senses"][0]["level"] = json!("Z9");
    let (status, _, problem) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": 2,
            "intent": "complete",
            "content": invalid
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(has_issue(&problem, "level_invalid"), "{problem}");
    assert!(
        problem["field_issues"]
            .as_array()
            .unwrap()
            .iter()
            .all(|issue| issue["schema_version"] == 3)
    );

    let (status, current) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{current}");
    assert_eq!(current["word"]["revision"], 2);

    let mut legal_content = complete_v3_meanings_fixture(pos_id);
    let mut legal_sentence = sentence;
    legal_sentence["links"][0]["sense_id"] = legal_content["pos"][0]["senses"][0]["id"].clone();
    legal_content["pos"][0]["senses"][0]["sentences"] = json!([legal_sentence]);
    let (status, complete) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 2,
            "intent": "complete",
            "content": legal_content
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{complete}");
    assert_eq!(complete["word"]["revision"], 3);
    assert!(
        complete["word"]["completed_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step == "meanings")
    );
    let sentence = &complete["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0];
    assert_eq!(sentence["associations"], json!([]));
    assert_eq!(sentence["associations_state"], "unresolved");
}

#[sqlx::test]
async fn v3_aggregate_node_limit_is_enforced_across_forms_and_meanings(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let saved = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = saved["word"]["id"].as_str().unwrap();

    let groups = (0..1_984)
        .map(|index| {
            json!({
                "id": Uuid::now_v7(),
                "name_zh": format!("义项 {index}"),
                "name_en": format!("sense {index}")
            })
        })
        .collect::<Vec<_>>();
    let (status, at_limit) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 2,
            "intent": "save",
            "content": {"sense_groups": groups, "pos": []}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{at_limit}");
    assert_eq!(at_limit["word"]["revision"], 3);

    let mut over_limit_groups = at_limit["word"]["meanings"]["sense_groups"]
        .as_array()
        .unwrap()
        .clone();
    over_limit_groups.push(json!({
        "id": Uuid::now_v7(),
        "name_zh": "超限",
        "name_en": "over"
    }));
    let (status, _, problem) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": 3,
            "intent": "save",
            "content": {"sense_groups": over_limit_groups, "pos": []}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(has_issue(&problem, "content_limit_exceeded"), "{problem}");
    assert!(
        problem["field_issues"]
            .as_array()
            .unwrap()
            .iter()
            .all(|issue| issue["schema_version"] == 3)
    );

    let stored_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(entry_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_revision, 3, "超限请求不得产生部分写入");
}

#[sqlx::test]
async fn v2_and_v3_relations_resolve_native_v3_draft_presentation_and_staleness(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let target_entry_id = target_forms["word"]["id"].as_str().unwrap();
    let target_pos_id = target_forms["word"]["forms"]["pos"][0]["pos_id"].clone();
    let mut target_meanings = complete_v3_meanings_fixture(target_pos_id);
    let target_sense_id = target_meanings["pos"][0]["senses"][0]["id"].clone();
    let (status, target_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": target_forms["word"]["revision"],
            "intent": "complete",
            "content": target_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{target_saved}");
    let target_label = target_saved["word"]["presentation"]["label"].clone();

    let v2_source = create_ready_draft(
        &state,
        &pool,
        &bearer,
        &format!("v2source{}", admin_id.simple()),
    )
    .await;
    let v2_source_id = v2_source["word"]["id"].as_str().unwrap();
    let mut v2_meanings = v2_source["word"]["meanings"].clone();
    v2_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": target_entry_id,
        "target_sense_id": target_sense_id,
        "target_headword": "forged",
        "target_gloss": "forged",
        "score": "95.00"
    }]);
    let (status, v2_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v2_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": v2_source["word"]["revision"],
            "intent": "complete",
            "content": v2_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v2_saved}");
    let v2_relation = &v2_saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(v2_relation["target_headword"], target_label);
    assert_eq!(v2_relation["target_gloss"], "港口");

    let v3_source_forms = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let v3_source_id = v3_source_forms["word"]["id"].as_str().unwrap();
    let mut v3_source_meanings =
        complete_v3_meanings_fixture(v3_source_forms["word"]["forms"]["pos"][0]["pos_id"].clone());
    v3_source_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": target_entry_id,
        "target_sense_id": target_sense_id,
        "score": "95.00"
    }]);
    let (status, v3_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v3_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": v3_source_forms["word"]["revision"],
            "intent": "complete",
            "content": v3_source_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v3_saved}");
    let v3_relation = &v3_saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(v3_relation["target_headword"], target_label);
    assert_eq!(v3_relation["target_gloss"], "港口");

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "harbour"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let target_context = detection["surface_match_page"]["matched_entry_contexts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|context| context["entry_id"] == target_entry_id)
        .expect("V3 incumbent must have a surface context");
    assert_eq!(target_context["gloss_previews"], json!(["港口"]));
    assert_eq!(target_context["inbound_relations"]["total"], 2);
    let preview_source_ids = target_context["inbound_relations"]["previews"]
        .as_array()
        .unwrap()
        .iter()
        .map(|preview| preview["source_entry_id"].clone())
        .collect::<HashSet<_>>();
    assert_eq!(
        preview_source_ids,
        HashSet::from([json!(v2_source_id), json!(v3_source_id),]),
        "V2/V3 relation sources must retain their truthful presentations: {target_context}"
    );
    assert!(
        target_context["inbound_relations"]["previews"]
            .as_array()
            .unwrap()
            .iter()
            .any(|preview| {
                preview["source_entry_id"] == v2_source_id
                    && preview["source_presentation"]["strategy_version"]
                        == "legacy_v2_surface_adapter_v1"
            }),
        "legacy source presentation must be explicit: {target_context}"
    );
    assert!(
        target_context["inbound_relations"]["previews"]
            .as_array()
            .unwrap()
            .iter()
            .any(|preview| {
                preview["source_entry_id"] == v3_source_id
                    && preview["source_presentation"] == v3_saved["word"]["presentation"]
            }),
        "native V3 source must use its authoritative presentation: {target_context}"
    );
    let stale_create_token = detection["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .unwrap()
        .to_owned();

    let mut v2_context_changed = v2_saved["word"]["meanings"].clone();
    v2_context_changed["pos"][0]["senses"][0]["relations"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": Uuid::now_v7(),
            "relation": "antonym",
            "target_word_id": target_entry_id,
            "target_sense_id": target_sense_id,
            "score": "90.00"
        }));
    let (status, v2_context_changed) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v2_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": v2_saved["word"]["revision"],
            "intent": "complete",
            "content": v2_context_changed
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v2_context_changed}");

    let entries_before_create: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    let create_key = Uuid::now_v7();
    let (status, stale_context) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(create_key),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word",
            "confirmed_surface_match_token": stale_create_token
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale_context}");
    assert_eq!(stale_context["code"], "surface_matches_changed");
    let changed_target_context =
        stale_context["meta"]["surface_match_page"]["matched_entry_contexts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|context| context["entry_id"] == target_entry_id)
            .expect("refreshed page must retain the V3 incumbent context");
    assert_eq!(changed_target_context["inbound_relations"]["total"], 3);
    let entries_after_rejected_create: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(entries_after_rejected_create, entries_before_create);
    let refreshed_create_token =
        stale_context["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
    let (status, created_after_context_ack) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(create_key),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word",
            "confirmed_surface_match_token": refreshed_create_token
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created_after_context_ack}");

    let restoring = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let restoring_id = restoring["word"]["id"].as_str().unwrap();
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{restoring_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": restoring["word"]["revision"],
            "base_lifecycle_revision": restoring["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    let restore_key = Uuid::now_v7();
    let restore_body = json!({
        "base_revision": archived["word"]["revision"],
        "base_lifecycle_revision": archived["word"]["lifecycle_revision"]
    });
    let (status, restore_required) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{restoring_id}/restore"),
        &bearer,
        Some(restore_key),
        Some(restore_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{restore_required}");
    let restore_target_context =
        restore_required["meta"]["surface_match_page"]["matched_entry_contexts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|context| context["entry_id"] == target_entry_id)
            .expect("restore page must carry the incumbent relation context");
    assert_eq!(restore_target_context["inbound_relations"]["total"], 3);
    let stale_restore_token =
        restore_required["meta"]["surface_match_page"]["surface_confirmation_token"].clone();

    let mut restore_context_changed = v2_context_changed["word"]["meanings"].clone();
    restore_context_changed["pos"][0]["senses"][0]["relations"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": Uuid::now_v7(),
            "relation": "derivative",
            "target_word_id": target_entry_id,
            "target_sense_id": target_sense_id,
            "score": "85.00"
        }));
    let (status, restore_context_changed) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v2_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": v2_context_changed["word"]["revision"],
            "intent": "complete",
            "content": restore_context_changed
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restore_context_changed}");
    let mut stale_restore_body = restore_body.clone();
    stale_restore_body["confirmed_surface_match_token"] = stale_restore_token;
    let (status, restore_changed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{restoring_id}/restore"),
        &bearer,
        Some(restore_key),
        Some(stale_restore_body),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{restore_changed}");
    assert_eq!(restore_changed["code"], "surface_matches_changed");
    let refreshed_restore_context =
        restore_changed["meta"]["surface_match_page"]["matched_entry_contexts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|context| context["entry_id"] == target_entry_id)
            .expect("changed restore page must retain the incumbent context");
    assert_eq!(refreshed_restore_context["inbound_relations"]["total"], 4);
    let unchanged_restore: (i64, bool) = sqlx::query_as(
        "SELECT lifecycle_revision, archived_at IS NOT NULL FROM lexicon.entries WHERE id = $1",
    )
    .bind(Uuid::parse_str(restoring_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unchanged_restore, (2, true));
    let mut refreshed_restore_body = restore_body;
    refreshed_restore_body["confirmed_surface_match_token"] =
        restore_changed["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
    let (status, restored) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{restoring_id}/restore"),
        &bearer,
        Some(restore_key),
        Some(refreshed_restore_body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["word"]["lifecycle_revision"], 3);

    target_meanings["pos"][0]["senses"][0]["definitions"][0]["content"]["text"] = json!("码头");
    let (status, target_updated) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{target_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": target_saved["word"]["revision"],
            "intent": "complete",
            "content": target_meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{target_updated}");

    let (status, validation) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{v3_source_id}/validate"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": v3_saved["word"]["revision"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{validation}");
    assert_eq!(validation["valid"], false);
    assert!(
        validation["issues"].as_array().is_some_and(|issues| issues
            .iter()
            .any(|issue| issue["code"] == "relation_target_stale")),
        "{validation}"
    );
}

#[sqlx::test]
async fn migrated_verified_v3_canary_publish_and_dual_version_activation(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let v2_published = create_and_publish(&state, &pool, &bearer, "quayside").await;
    let entry_id = Uuid::parse_str(v2_published["word"]["id"].as_str().unwrap()).unwrap();
    let (v2_publication_id, v2_snapshot, v2_hash): (Uuid, Value, Vec<u8>) = sqlx::query_as(
        r#"
        SELECT publication.id, publication.snapshot, publication.snapshot_hash
        FROM lexicon.entries entry
        JOIN lexicon.entry_publications publication
          ON publication.id = entry.current_publication_id
        WHERE entry.id = $1
        "#,
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let batch_id = Uuid::now_v7();
    let dry_run_report = dry_run(&pool, batch_id, admin_id, Uuid::now_v7(), &[entry_id])
        .await
        .unwrap();
    approve(
        &pool,
        batch_id,
        admin_id,
        Uuid::now_v7(),
        &dry_run_report.manifest_digest,
    )
    .await
    .unwrap();
    let applied = apply(
        &pool,
        batch_id,
        admin_id,
        Uuid::now_v7(),
        &dry_run_report.manifest_digest,
    )
    .await
    .unwrap();
    assert_eq!(applied.applied_entries, 1);
    let verified = verify(&pool, batch_id, admin_id, Uuid::now_v7())
        .await
        .unwrap();
    assert!(verified.ready, "{verified:?}");

    let mut migration_barrier = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("lexicon.v3-migration.entry:{entry_id}"))
        .execute(&mut *migration_barrier)
        .await
        .unwrap();
    let canary_pool = pool.clone();
    let enable_task = tokio::spawn(async move {
        enable_publication_canary(&canary_pool, batch_id, entry_id, admin_id, Uuid::now_v7()).await
    });
    let concurrent_state = state.clone();
    let concurrent_bearer = bearer.clone();
    let publish_task = tokio::spawn(async move {
        call(
            &concurrent_state,
            Method::POST,
            &format!("{ROOT}/entries/{entry_id}/publications"),
            &concurrent_bearer,
            Some(Uuid::now_v7()),
            Some(json!({"schema_version": 3, "base_revision": 999})),
        )
        .await
    });
    await_database_lock_waiters(&pool, 2).await;
    migration_barrier.commit().await.unwrap();
    let enabled = enable_task.await.unwrap();
    assert!(
        enabled.is_ok(),
        "canary enable must not deadlock: {enabled:?}"
    );
    let (concurrent_status, concurrent_publish) = publish_task.await.unwrap();
    assert_eq!(
        concurrent_status,
        StatusCode::CONFLICT,
        "concurrent canary/publish must fail stably rather than deadlock: {concurrent_publish}"
    );

    let (status, migrated) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{migrated}");
    assert_eq!(migrated["word"]["schema_version"], 3);
    assert!(migrated["word"]["compatibility"]["legacy_headwords"].is_object());

    let publish_key = Uuid::now_v7();
    let publish_body = json!({
        "schema_version": 3,
        "base_revision": migrated["word"]["revision"]
    });
    let mut successful_publish_body = publish_body.clone();
    let (mut status, mut v3_published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(publish_key),
        Some(publish_body.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT {
        let token = v3_published["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .expect("canary publish conflict must return a confirmation token");
        let mut confirmed = publish_body.clone();
        confirmed["confirmed_surface_match_token"] = json!(token);
        successful_publish_body = confirmed.clone();
        (status, v3_published) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/entries/{entry_id}/publications"),
            &bearer,
            Some(publish_key),
            Some(confirmed),
        )
        .await;
    }
    assert_eq!(status, StatusCode::CREATED, "{v3_published}");
    assert_eq!(v3_published["word"]["schema_version"], 3);
    let v3_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(v3_publication_id, v2_publication_id);
    let v3_publication_schema: i16 = sqlx::query_scalar(
        "SELECT content_schema_version FROM lexicon.entry_publications WHERE id = $1",
    )
    .bind(v3_publication_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(v3_publication_schema, 3);
    let read_disabled_state = state
        .clone()
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::default());
    let (status, hidden_v3_history) = call(
        &read_disabled_state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hidden_v3_history}");
    assert_eq!(
        hidden_v3_history["publications"].as_array().unwrap().len(),
        1
    );
    assert_eq!(hidden_v3_history["publications"][0]["schema_version"], 2);
    let (status, _, hidden_v3_detail) = call_problem(
        &read_disabled_state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}/publications/{v3_publication_id}"),
        &bearer,
        None,
        json!(null),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{hidden_v3_detail}"
    );
    let (status, hidden_v3_stats) = call(
        &read_disabled_state,
        Method::GET,
        &format!("{ROOT}/entries/stats"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hidden_v3_stats}");
    assert_eq!(hidden_v3_stats["total"], 0);
    let (status, visible_v3_stats) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/stats"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{visible_v3_stats}");
    assert_eq!(visible_v3_stats["total"], 1);

    let (stored_v2_snapshot, stored_v2_hash): (Value, Vec<u8>) = sqlx::query_as(
        "SELECT snapshot, snapshot_hash FROM lexicon.entry_publications WHERE id = $1",
    )
    .bind(v2_publication_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored_v2_snapshot, v2_snapshot);
    assert_eq!(stored_v2_hash, v2_hash);

    let (status, replayed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(publish_key),
        Some(successful_publish_body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replayed}");
    assert_eq!(
        replayed["word"]["published_revision"],
        v3_published["word"]["published_revision"]
    );

    let activate_key = Uuid::now_v7();
    let mut activate_body = json!({
        "schema_version": 3,
        "base_revision": v3_published["word"]["revision"],
        "base_lifecycle_revision": v3_published["word"]["lifecycle_revision"]
    });
    let activate_path =
        format!("{ROOT}/entries/{entry_id}/publications/{v2_publication_id}/activate");
    let (mut status, mut activated_v2) = call(
        &state,
        Method::POST,
        &activate_path,
        &bearer,
        Some(activate_key),
        Some(activate_body.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT {
        let token = activated_v2["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .expect("V2 history activation conflict must return a confirmation token");
        activate_body["confirmed_surface_match_token"] = json!(token);
        (status, activated_v2) = call(
            &state,
            Method::POST,
            &activate_path,
            &bearer,
            Some(activate_key),
            Some(activate_body),
        )
        .await;
    }
    assert_eq!(status, StatusCode::OK, "{activated_v2}");
    let current_after_v2: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current_after_v2, v2_publication_id);
    let v2_surface_event_schema: i64 = sqlx::query_scalar(
        r#"
        SELECT (payload ->> 'content_schema_version')::BIGINT
        FROM platform.outbox_events
        WHERE aggregate_id = $1
          AND event_type = 'lexicon.surface_projection_replaced'
          AND payload ->> 'publication_id' = $2
        ORDER BY occurred_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(entry_id)
    .bind(v2_publication_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(v2_surface_event_schema, 2);

    let activate_v3_key = Uuid::now_v7();
    let mut activate_v3_body = json!({
        "schema_version": 3,
        "base_revision": activated_v2["word"]["revision"],
        "base_lifecycle_revision": activated_v2["word"]["lifecycle_revision"]
    });
    let activate_v3_path =
        format!("{ROOT}/entries/{entry_id}/publications/{v3_publication_id}/activate");
    let (mut status, mut activated_v3) = call(
        &state,
        Method::POST,
        &activate_v3_path,
        &bearer,
        Some(activate_v3_key),
        Some(activate_v3_body.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT {
        let token = activated_v3["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .expect("V3 history activation conflict must return a confirmation token");
        activate_v3_body["confirmed_surface_match_token"] = json!(token);
        (status, activated_v3) = call(
            &state,
            Method::POST,
            &activate_v3_path,
            &bearer,
            Some(activate_v3_key),
            Some(activate_v3_body),
        )
        .await;
    }
    assert_eq!(status, StatusCode::OK, "{activated_v3}");
    let current_after_v3: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current_after_v3, v3_publication_id);
    let mut invalid_snapshot: Value =
        sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id = $1")
            .bind(v3_publication_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    invalid_snapshot["forms"]["pos"][0]["dialect_rules"] = json!({
        "spelling_mode": "distinguish",
        "phonetic_mode": "unified"
    });
    let invalid_hash = sha256_json(&invalid_snapshot).unwrap();
    sqlx::query(
        "UPDATE lexicon.entry_publications SET snapshot = $2, snapshot_hash = $3 WHERE id = $1",
    )
    .bind(v3_publication_id)
    .bind(invalid_snapshot)
    .bind(invalid_hash)
    .execute(&pool)
    .await
    .unwrap();
    let (status, _, invalid_activation) = call_problem(
        &state,
        Method::POST,
        &activate_v3_path,
        &bearer,
        Some(Uuid::now_v7()),
        json!({
            "schema_version": 3,
            "base_revision": activated_v3["word"]["revision"],
            "base_lifecycle_revision": activated_v3["word"]["lifecycle_revision"]
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{invalid_activation}"
    );
    let rule_issue = invalid_activation["field_issues"]
        .as_array()
        .and_then(|issues| {
            issues
                .iter()
                .find(|issue| issue["code"] == "dialect_rules_invalid")
        })
        .expect("history activation 必须复核 V3 dialect_rules");
    assert_eq!(rule_issue["field"], "dialect_rules");
    assert_eq!(rule_issue["node_id"], rule_issue["node_location"]["pos_id"]);
    let v3_surface_event_schema: i64 = sqlx::query_scalar(
        r#"
        SELECT (payload ->> 'content_schema_version')::BIGINT
        FROM platform.outbox_events
        WHERE aggregate_id = $1
          AND event_type = 'lexicon.surface_projection_replaced'
          AND payload ->> 'publication_id' = $2
        ORDER BY occurred_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(entry_id)
    .bind(v3_publication_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(v3_surface_event_schema, 3);

    let (stored_v2_snapshot_after, stored_v2_hash_after): (Value, Vec<u8>) = sqlx::query_as(
        "SELECT snapshot, snapshot_hash FROM lexicon.entry_publications WHERE id = $1",
    )
    .bind(v2_publication_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored_v2_snapshot_after, v2_snapshot);
    assert_eq!(stored_v2_hash_after, v2_hash);

    let terminal_verification = verify(&pool, batch_id, admin_id, Uuid::now_v7())
        .await
        .unwrap();
    assert!(terminal_verification.ready, "{terminal_verification:?}");
    assert_eq!(terminal_verification.checked_entries, 1);
    assert_eq!(terminal_verification.verified_entries, 1);
    assert_eq!(terminal_verification.entries[0].status, "verified");
}

#[sqlx::test]
async fn v3_historical_activation_revalidates_outbound_and_inbound_sense_refs(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let outbound_target =
        create_and_publish(&state, &pool, &bearer, "v3-activation-outbound-target").await;
    let outbound_target_id =
        Uuid::parse_str(outbound_target["word"]["id"].as_str().unwrap()).unwrap();
    let outbound_target_sense_id =
        outbound_target["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();

    let initial = create_and_publish(&state, &pool, &bearer, "v3-activation-ref-guard").await;
    let entry_id = Uuid::parse_str(initial["word"]["id"].as_str().unwrap()).unwrap();
    let inbound_guard_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    let mut with_outbound = initial["word"]["meanings"].clone();
    with_outbound["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": outbound_target_id,
        "target_sense_id": outbound_target_sense_id,
        "score": "80.00"
    }]);
    let (status, outbound_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": initial["word"]["revision"],
            "intent": "complete",
            "content": with_outbound,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "保存历史出站引用失败：{outbound_saved}"
    );
    let (status, outbound_published) = publish_ready(&state, &bearer, &outbound_saved).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "发布出站引用版本失败：{outbound_published}"
    );
    let outbound_guard_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    let mut current_meanings = outbound_published["word"]["meanings"].clone();
    current_meanings["pos"][0]["senses"][0]["relations"] = json!([]);
    let mut added_sense = current_meanings["pos"][0]["senses"][0].clone();
    let added_sense_id = Uuid::now_v7();
    added_sense["id"] = json!(added_sense_id);
    added_sense["definitions"][0]["id"] = json!(Uuid::now_v7());
    added_sense["definitions"][0]["content_id"] = json!(Uuid::now_v7());
    added_sense["definitions"][0]["content"] = rich_text("仅当前发布版本保留的词义");
    added_sense["sentences"][0]["id"] = json!(Uuid::now_v7());
    added_sense["sentences"][0]["en_text"]["common"]["id"] = json!(Uuid::now_v7());
    added_sense["sentences"][0]["zh_text_id"] = json!(Uuid::now_v7());
    added_sense["sentences"][0]["links"][0]["sense_id"] = json!(added_sense_id);
    added_sense["relations"] = json!([]);
    current_meanings["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .push(added_sense);
    let (status, current_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": outbound_published["word"]["revision"],
            "intent": "complete",
            "content": current_meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "保存当前版本新词义失败：{current_saved}"
    );
    let (status, current_published) = publish_ready(&state, &bearer, &current_saved).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "发布当前版本失败：{current_published}"
    );
    let current_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    let inbound_source =
        create_ready_draft(&state, &pool, &bearer, "v3-activation-inbound-source").await;
    let inbound_source_id = inbound_source["word"]["id"].as_str().unwrap();
    let mut inbound_source_meanings = inbound_source["word"]["meanings"].clone();
    inbound_source_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": entry_id,
        "target_sense_id": added_sense_id,
        "score": "85.00"
    }]);
    let (status, inbound_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{inbound_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": inbound_source["word"]["revision"],
            "intent": "complete",
            "content": inbound_source_meanings,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "保存当前入站引用失败：{inbound_saved}"
    );
    let (status, inbound_published) = publish_ready(&state, &bearer, &inbound_saved).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "发布当前入站引用失败：{inbound_published}"
    );

    let (status, archived_target) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{outbound_target_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": outbound_target["word"]["revision"],
            "base_lifecycle_revision": outbound_target["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "当前版本无引用时应可归档历史目标：{archived_target}"
    );

    let batch_id = Uuid::now_v7();
    let plan = dry_run(&pool, batch_id, admin_id, Uuid::now_v7(), &[entry_id])
        .await
        .unwrap();
    approve(
        &pool,
        batch_id,
        admin_id,
        Uuid::now_v7(),
        &plan.manifest_digest,
    )
    .await
    .unwrap();
    apply(
        &pool,
        batch_id,
        admin_id,
        Uuid::now_v7(),
        &plan.manifest_digest,
    )
    .await
    .unwrap();
    assert!(
        verify(&pool, batch_id, admin_id, Uuid::now_v7())
            .await
            .unwrap()
            .ready
    );
    enable_publication_canary(&pool, batch_id, entry_id, admin_id, Uuid::now_v7())
        .await
        .unwrap();

    let (status, migrated) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{migrated}");
    assert_eq!(migrated["word"]["schema_version"], 3);
    let base_revision = migrated["word"]["revision"].as_i64().unwrap();
    let base_lifecycle_revision = migrated["word"]["lifecycle_revision"].as_i64().unwrap();

    let before_inbound = activation_write_fingerprint(&pool, entry_id).await;
    let (status, inbound_blocked) = activate_v3_history(
        &state,
        &bearer,
        entry_id,
        inbound_guard_publication_id,
        base_revision,
        base_lifecycle_revision,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "目标快照丢失仍被当前 publication 引用的 sense 时必须 fail closed：{inbound_blocked}"
    );
    assert!(
        inbound_blocked["field_issues"]
            .as_array()
            .is_some_and(|issues| issues.iter().any(|issue| {
                issue["schema_version"] == 3
                    && issue["code"] == "sense_has_inbound_publication_refs"
                    && issue["node_id"] == added_sense_id.to_string()
            }))
    );
    assert_eq!(
        activation_write_fingerprint(&pool, entry_id).await,
        before_inbound,
        "入站引用校验失败不得切 pointer、替换 surface 或写 audit/outbox/idempotency"
    );

    let before_outbound = activation_write_fingerprint(&pool, entry_id).await;
    let (status, outbound_blocked) = activate_v3_history(
        &state,
        &bearer,
        entry_id,
        outbound_guard_publication_id,
        base_revision,
        base_lifecycle_revision,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "历史 publication 的出站目标已归档时必须 fail closed：{outbound_blocked}"
    );
    assert_eq!(
        outbound_blocked["code"],
        "entry_has_unavailable_publication_refs"
    );
    assert_eq!(
        activation_write_fingerprint(&pool, entry_id).await,
        before_outbound,
        "出站引用校验失败不得切 pointer、替换 surface 或写 audit/outbox/idempotency"
    );
    let pointer_after_failures: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pointer_after_failures, current_publication_id);
}

#[sqlx::test]
async fn v3_detection_and_create_use_kaikki_forms_and_ipa_evidence(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    seed_dictionary_word(&pool, "child").await;
    let dataset_id: i64 =
        sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status = 'active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.content_imports (
            dataset_id, input_sha256, source_locator, source_version,
            record_count, parser_version
        ) VALUES (
            $1, repeat('a', 64), 'https://kaikki.org/test-source',
            'enwiktionary-content-test', 1, 'forms-sounds-v1'
        )
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO dictionary.entry_contents (
            dataset_id, source_key, normalized_term, pos, senses,
            forms, sounds, source_locator
        ) VALUES (
            $1, 'kaikki:child:noun:test', 'child', 'noun', '[]'::jsonb,
            $2, $3, 'https://kaikki.org/dictionary/English/meaning/c/ch/child.html'
        )
        "#,
    )
    .bind(dataset_id)
    .bind(json!([{"form": "children", "tags": ["plural"]}]))
    .bind(json!([{"ipa": "/tʃaɪld/"}]))
    .execute(&pool)
    .await
    .unwrap();

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "child"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    assert_eq!(
        detection["builtin_dictionary"]["coverage"]["forms"],
        "partial"
    );
    assert_eq!(
        detection["builtin_dictionary"]["coverage"]["pronunciations"],
        "partial"
    );
    assert_eq!(
        detection["builtin_dictionary"]["provenance"]["pronunciations"],
        json!({"name": "test", "version": "enwiktionary-content-test"})
    );
    assert_eq!(
        detection["builtin_dictionary"]["provenance"]["forms"],
        json!({"name": "test", "version": "enwiktionary-content-test"})
    );
    let suggestions = detection["builtin_dictionary"]["suggested_forms"]
        .as_array()
        .unwrap();
    assert_eq!(suggestions.len(), 2, "{suggestions:?}");
    assert_eq!(suggestions[0]["form_type"], "base");
    assert_eq!(
        suggestions[0]["regional_variants"]["common"]["pronunciations"][0]["dict_phonetic"],
        "/tʃaɪld/"
    );
    assert!(
        suggestions[0]["regional_variants"]["common"]["pronunciations"][0]
            .get("actual_pron")
            .is_none()
    );
    assert_eq!(suggestions[1]["form_type"], "plural");
    assert_eq!(
        suggestions[1]["regional_variants"]["common"]["spelling"],
        "children"
    );

    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let forms = created["word"]["forms"]["pos"][0]["forms"]
        .as_array()
        .unwrap();
    assert_eq!(forms.len(), 2, "{forms:?}");
    assert_eq!(forms[1]["form_type"], "plural");
    assert_eq!(
        forms[1]["regional_variants"]["common"]["spelling"],
        "children"
    );
    assert_eq!(
        forms[0]["regional_variants"]["common"]["pronunciations"][0]["dict_phonetic"],
        "/tʃaɪld/"
    );
    assert_eq!(
        forms[0]["regional_variants"]["common"]["pronunciations"][0]["actual_pron"],
        ""
    );
}

#[sqlx::test]
async fn v3_create_materializes_builtin_and_existing_pos_suggestions(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    seed_dictionary_word(&pool, "center").await;

    let (status, first_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "center"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first_detection}");
    assert_eq!(first_detection["suggested_pos"], json!(["noun"]));
    assert_eq!(
        first_detection["builtin_dictionary"]["suggested_pos"],
        json!(["noun"])
    );

    let (status, first_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": first_detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first_created}");
    assert_eq!(
        first_created["word"]["forms"]["pos"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(first_created["word"]["forms"]["pos"][0]["pos"], "noun");
    assert_eq!(
        first_created["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["common"]["spelling"],
        "center"
    );

    let first_entry_id = Uuid::parse_str(first_created["word"]["id"].as_str().unwrap()).unwrap();
    let mut first_forms = first_created["word"]["forms"].clone();
    let pronoun_pos_id = Uuid::now_v7();
    let pronoun_group_id = Uuid::now_v7();
    let pronoun_form_id = Uuid::now_v7();
    first_forms["pos"].as_array_mut().unwrap().push(json!({
        "pos_id": pronoun_pos_id,
        "pos": "pronoun",
        "dialect_rules": {
            "spelling_mode": "unified",
            "phonetic_mode": "unified"
        },
        "forms": [{
            "id": pronoun_form_id,
            "form_type": "base",
            "regional_variants": {
                "mode": "common",
                "common": {
                    "id": Uuid::now_v7(),
                    "dialect": "common",
                    "spelling": "",
                    "origin": "manual",
                    "pronunciations": [{
                        "id": Uuid::now_v7(),
                        "dict_phonetic": "",
                        "actual_pron": "",
                        "style": "normal"
                    }]
                }
            }
        }],
        "form_groups": [{
            "id": pronoun_group_id,
            "is_regular": true,
            "members": [{"id": Uuid::now_v7(), "form_id": pronoun_form_id}]
        }]
    }));
    let (status, first_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{first_entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "save",
            "content": first_forms
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first_saved}");

    let (status, duplicate_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "center"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{duplicate_detection}");
    assert_eq!(duplicate_detection["requires_acknowledgement"], true);
    assert_eq!(
        duplicate_detection["suggested_pos"],
        json!(["noun", "pronoun"])
    );
    assert_eq!(
        duplicate_detection["builtin_dictionary"]["suggested_pos"],
        json!(["noun"]),
        "existing-entry POS must not contaminate builtin provenance"
    );

    let duplicate_create = json!({
        "schema_version": 3,
        "detection_id": duplicate_detection["detection_id"],
        "kind": "word"
    });
    let (status, _, required) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        duplicate_create.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{required}");
    let mut confirmed = duplicate_create;
    confirmed["confirmed_surface_match_token"] =
        required["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
    let (status, second_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(confirmed),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second_created}");
    let second_forms = &second_created["word"]["forms"];
    assert_eq!(
        second_forms["pos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pos| pos["pos"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["noun", "pronoun"]
    );
    let existing_only = &second_forms["pos"][1];
    assert_eq!(
        existing_only["forms"][0]["regional_variants"]["common"]["spelling"],
        ""
    );
    assert_eq!(
        existing_only["forms"][0]["regional_variants"]["common"]["pronunciations"][0]["dict_phonetic"],
        ""
    );
    assert_eq!(
        existing_only["forms"][0]["regional_variants"]["common"]["pronunciations"][0]["actual_pron"],
        ""
    );
    assert!(
        json_uuids(&first_saved["word"]["forms"]).is_disjoint(&json_uuids(second_forms)),
        "new entry must not reuse any existing forms node UUID"
    );

    let second_entry_id = Uuid::parse_str(second_created["word"]["id"].as_str().unwrap()).unwrap();
    let counts: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
            (SELECT count(*) FROM lexicon.entry_pos WHERE entry_id = $1),
            (SELECT count(*) FROM lexicon.v3_form_groups WHERE entry_id = $1),
            (SELECT count(*) FROM lexicon.v3_concrete_forms WHERE entry_id = $1),
            (SELECT count(*) FROM lexicon.v3_group_memberships WHERE entry_id = $1),
            (SELECT count(*) FROM lexicon.v3_form_variants WHERE entry_id = $1),
            (SELECT count(*) FROM lexicon.v3_pronunciations WHERE entry_id = $1)
        "#,
    )
    .bind(second_entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (2, 2, 2, 2, 2, 2));

    let (status, read_back) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{second_entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read_back}");
    assert_eq!(read_back["word"]["forms"], *second_forms);
}

#[sqlx::test]
async fn v3_create_not_found_without_existing_pos_stays_blank(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "no-suggestion-surface"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    assert_eq!(detection["builtin_dictionary"]["status"], "not_found");
    assert_eq!(detection["suggested_pos"], json!([]));

    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["word"]["forms"]["pos"], json!([]));
    let entry_id = Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap();
    let counts: (i64, i64) = sqlx::query_as(
        r#"
        SELECT
            (SELECT count(*) FROM lexicon.entry_pos WHERE entry_id = $1),
            (SELECT count(*) FROM lexicon.v3_concrete_forms WHERE entry_id = $1)
        "#,
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (0, 0));

    let (status, detail) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "空骨架必须仍可按 ID 编辑：{detail}");
    let (status, list) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?page=1&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["words"], json!([]), "纯空骨架不得进入主列表：{list}");
    assert_eq!(list["page"]["total"], 0, "分页总数必须与主列表一致：{list}");
    let (status, stats) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/stats"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_eq!(stats["total"], 0, "纯空骨架不得进入统计：{stats}");
}

#[sqlx::test]
async fn v3_phrase_detection_and_creation_use_native_aggregate(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_draft =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["The harbour is calm."])
            .await;
    let (status, target_published) = publish_ready_v3(&state, &bearer, &target_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{target_published}");
    let target_entry_id = target_published["word"]["id"].as_str().unwrap();
    let target_entry_uuid = Uuid::parse_str(target_entry_id).unwrap();
    let target_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(target_entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    let target_pos_id = target_published["word"]["forms"]["pos"][0]["pos_id"].clone();
    let target_form = &target_published["word"]["forms"]["pos"][0]["forms"][0];
    let target_form_id = target_form["id"].clone();
    let target_uk_variant_id = target_form["regional_variants"]["uk"]["id"].clone();
    let target_us_variant_id = target_form["regional_variants"]["us"]["id"].clone();
    let target_sense_id = target_published["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();
    let target_headword = target_published["word"]["presentation"]["label"].clone();
    let target_gloss = target_published["word"]["meanings"]["pos"][0]["senses"][0]["definitions"]
        [0]["content"]["text"]
        .clone();

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "phrase",
            "surface": "native phrase"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    assert_eq!(detection["request"]["kind"], "phrase");
    assert_eq!(detection["normalized_surface"], "native phrase");
    assert_eq!(detection["builtin_dictionary"]["status"], "not_found");

    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "phrase",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["word"]["schema_version"], 3);
    assert_eq!(created["word"]["kind"], "phrase");
    assert_eq!(created["word"]["forms"], json!({"pos": []}));
    let entry_id = created["word"]["id"].as_str().unwrap();
    let entry_uuid = Uuid::parse_str(entry_id).unwrap();

    let (status, read_back) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read_back}");
    assert_eq!(read_back["word"]["schema_version"], 3);
    assert_eq!(read_back["word"]["kind"], "phrase");

    let mut forms = complete_v3_forms_fixture();
    for form in forms["pos"][0]["forms"].as_array_mut().unwrap() {
        form["regional_variants"]["uk"]["spelling"] = json!("native phrase");
        form["regional_variants"]["us"]["spelling"] = json!("native phrase");
    }
    let uk_component_id = Uuid::now_v7();
    let us_component_id = Uuid::now_v7();
    forms["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"] = json!([{
        "id": uk_component_id,
        "state": "resolved",
        "literal": "native",
        "target_word_id": target_entry_id,
        "target_publication_id": target_publication_id,
        "target_pos_id": target_pos_id,
        "target_base_form_id": target_form_id,
        "target_sense_id": target_sense_id,
        "target_form_id": target_form_id,
        "target_variant_id": target_uk_variant_id,
        "target_dialect": "uk",
        "target_form_type": "base",
        "target_headword": target_headword,
        "target_gloss": target_gloss
    }]);
    forms["pos"][0]["forms"][0]["regional_variants"]["us"]["component_usages"] = json!([{
        "id": us_component_id,
        "state": "resolved",
        "literal": "phrase",
        "target_word_id": target_entry_id,
        "target_publication_id": target_publication_id,
        "target_pos_id": target_pos_id,
        "target_base_form_id": target_form_id,
        "target_sense_id": target_sense_id,
        "target_form_id": target_form_id,
        "target_variant_id": target_us_variant_id,
        "target_dialect": "us",
        "target_form_type": "base",
        "target_headword": target_headword,
        "target_gloss": target_gloss
    }]);
    let (_, forms_saved) =
        save_v3_forms_after_impact(&state, &bearer, entry_id, 1, "complete", forms).await;
    assert_eq!(forms_saved["word"]["kind"], "phrase");
    assert_eq!(
        forms_saved["word"]["presentation"]["matched_surfaces"],
        json!(["native phrase"])
    );
    assert_eq!(
        forms_saved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"]
            [0]["literal"],
        "native"
    );
    assert_eq!(
        forms_saved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["us"]["component_usages"]
            [0]["literal"],
        "phrase"
    );

    let original_forms = forms_saved["word"]["forms"].clone();
    let mut compatibility_forms = original_forms.clone();
    for form in compatibility_forms["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
    {
        for dialect in ["uk", "us"] {
            form["regional_variants"][dialect]
                .as_object_mut()
                .unwrap()
                .remove("component_usages");
        }
    }
    let (_, compatibility_saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        entry_id,
        forms_saved["word"]["revision"].as_i64().unwrap(),
        "complete",
        compatibility_forms,
    )
    .await;
    assert_eq!(
        compatibility_saved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"]
            [0]["id"],
        uk_component_id.to_string(),
        "旧客户端缺少 component_usages 时必须保留具体方言侧配置"
    );
    assert_eq!(
        compatibility_saved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["us"]["component_usages"]
            [0]["id"],
        us_component_id.to_string()
    );

    let mut explicit_clear = compatibility_saved["word"]["forms"].clone();
    for dialect in ["uk", "us"] {
        explicit_clear["pos"][0]["forms"][0]["regional_variants"][dialect]["component_usages"] =
            json!([]);
    }
    let (_, explicitly_cleared) = save_v3_forms_after_impact(
        &state,
        &bearer,
        entry_id,
        compatibility_saved["word"]["revision"].as_i64().unwrap(),
        "complete",
        explicit_clear,
    )
    .await;
    for dialect in ["uk", "us"] {
        assert_eq!(
            explicitly_cleared["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"][dialect]
                ["component_usages"],
            json!([]),
            "显式空数组必须主动清空成分用词"
        );
    }

    let (_, forms_saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        entry_id,
        explicitly_cleared["word"]["revision"].as_i64().unwrap(),
        "complete",
        original_forms,
    )
    .await;

    let stored_components: Vec<(String, Uuid, String)> = sqlx::query_as(
        r#"
        SELECT variant.dialect, component.id, component.literal
        FROM lexicon.v3_phrase_variant_component_usages component
        JOIN lexicon.v3_form_variants variant
          ON variant.id = component.form_variant_id
         AND variant.entry_id = component.entry_id
        WHERE component.entry_id = $1
        ORDER BY variant.dialect, component.ordinal
        "#,
    )
    .bind(entry_uuid)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        stored_components,
        vec![
            ("uk".to_owned(), uk_component_id, "native".to_owned()),
            ("us".to_owned(), us_component_id, "phrase".to_owned())
        ]
    );

    let projected_kinds: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT entry_kind
        FROM lexicon.surface_sources
        WHERE entry_id = $1
          AND content_schema_version = 3
          AND content_scope = 'draft'
          AND is_deleted = FALSE
        ORDER BY source_id, dialect_scope
        "#,
    )
    .bind(entry_uuid)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(!projected_kinds.is_empty());
    assert!(projected_kinds.iter().all(|kind| kind == "phrase"));

    let (status, repeated_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "phrase",
            "surface": "native phrase"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repeated_detection}");
    assert!(
        repeated_detection["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["match_kind"] == "form_variant_v3"
                && item["match"]["entry_kind"] == "phrase")
    );

    let pos_id = forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone();
    let (status, meanings_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": forms_saved["word"]["revision"],
            "intent": "complete",
            "content": complete_v3_meanings_fixture(pos_id)
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{meanings_saved}");
    assert_eq!(meanings_saved["word"]["kind"], "phrase");

    let mut target_meanings = target_published["word"]["meanings"].clone();
    target_meanings["pos"][0]["senses"][0]["definitions"][0]["content"] = rich_text("新港口");
    let target_saved = save_v3_meanings(&state, &bearer, &target_published, target_meanings).await;
    let (status, target_republished) = publish_ready_v3(&state, &bearer, &target_saved).await;
    assert_eq!(status, StatusCode::CREATED, "{target_republished}");
    let current_target_publication: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(target_entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(current_target_publication, target_publication_id);

    let (_, phrase_resaved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        entry_id,
        meanings_saved["word"]["revision"].as_i64().unwrap(),
        "complete",
        meanings_saved["word"]["forms"].clone(),
    )
    .await;
    assert_eq!(
        phrase_resaved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"]
            [0]["target_publication_id"],
        target_publication_id.to_string(),
        "目标发布 B 后来源 resave 仍必须锚定历史 A"
    );

    let mut target_lock = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM lexicon.entries WHERE id = $1 FOR UPDATE")
        .bind(target_entry_uuid)
        .execute(&mut *target_lock)
        .await
        .unwrap();
    let (locked_status, locked_publish) = publish_ready_v3(&state, &bearer, &phrase_resaved).await;
    assert_eq!(locked_status, StatusCode::CONFLICT, "{locked_publish}");
    assert_eq!(locked_publish["code"], "reference_conflict");
    target_lock.rollback().await.unwrap();

    sqlx::query(
        "UPDATE lexicon.entries SET archived_at = now(), archived_by_admin_id = $2 WHERE id = $1",
    )
    .bind(target_entry_uuid)
    .bind(admin_id)
    .execute(&pool)
    .await
    .unwrap();
    let (archived_status, archived_publish) =
        publish_ready_v3(&state, &bearer, &phrase_resaved).await;
    assert_eq!(archived_status, StatusCode::CONFLICT, "{archived_publish}");
    assert_eq!(archived_publish["code"], "reference_conflict");
    sqlx::query(
        "UPDATE lexicon.entries SET archived_at = NULL, archived_by_admin_id = NULL WHERE id = $1",
    )
    .bind(target_entry_uuid)
    .execute(&pool)
    .await
    .unwrap();

    let (status, published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "base_revision": phrase_resaved["word"]["revision"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_eq!(published["word"]["kind"], "phrase");
    assert_eq!(published["word"]["status"], "published");

    assert_eq!(
        published["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"]
            [0]["literal"],
        "native"
    );
    assert_eq!(
        published["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["us"]["component_usages"]
            [0]["literal"],
        "phrase"
    );
    assert_eq!(
        published["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"]
            [0]["target_publication_id"],
        target_publication_id.to_string()
    );
    assert_eq!(
        published["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"]
            [0]["target_variant_id"],
        target_uk_variant_id
    );
    assert_eq!(
        published["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["us"]["component_usages"]
            [0]["target_variant_id"],
        target_us_variant_id
    );
    let publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    let published_component_nodes: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM lexicon.entry_publication_nodes
        WHERE publication_id = $1
          AND entry_id = $2
          AND node_type = 'phrase_component_usage'
        "#,
    )
    .bind(publication_id)
    .bind(entry_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(published_component_nodes, 2);
    let referenced_target_publications: Vec<Uuid> = sqlx::query_scalar(
        r#"
        SELECT target_publication_id
        FROM lexicon.entry_publication_sense_refs
        WHERE publication_id = $1
          AND reference_kind = 'phrase_component'
        ORDER BY source_node_id
        "#,
    )
    .bind(publication_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        referenced_target_publications,
        vec![target_publication_id, target_publication_id]
    );

    let published_surface_kinds: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT entry_kind
        FROM lexicon.surface_sources
        WHERE entry_id = $1
          AND content_schema_version = 3
          AND content_scope = 'current_publication'
          AND is_deleted = FALSE
        ORDER BY source_id, dialect_scope
        "#,
    )
    .bind(entry_uuid)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(!published_surface_kinds.is_empty());
    assert!(published_surface_kinds.iter().all(|kind| kind == "phrase"));

    let (status, discovered) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/sentence-targets/resolve"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "sentence_text": "A native phrase appears.",
            "source_dialect": "common",
            "mode": "all_published_targets",
            "page_size_per_range": 20
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{discovered}");
    let phrase_candidate = discovered["range_results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|range| range["published_matches"].as_array().unwrap())
        .find(|candidate| candidate["entry_id"] == entry_id)
        .expect("discovery must return the published phrase");
    assert_eq!(phrase_candidate["kind"], "phrase");
    let mut phrase_meanings = published["word"]["meanings"].clone();
    phrase_meanings["pos"][0]["senses"][0]["definitions"][0]["content"] = rich_text("短语新义");
    let phrase_saved = save_v3_meanings(&state, &bearer, &published, phrase_meanings).await;
    let (status, phrase_republished) = publish_ready_v3(&state, &bearer, &phrase_saved).await;
    assert_eq!(status, StatusCode::CREATED, "{phrase_republished}");
    let current_phrase_publication: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(current_phrase_publication, publication_id);

    let published = phrase_republished;

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
    assert_eq!(status, StatusCode::OK, "{archived}");
    assert_eq!(archived["word"]["schema_version"], 3);
    assert_eq!(archived["word"]["kind"], "phrase");
    assert_eq!(archived["word"]["status"], "archived");

    let (status, restored) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": archived["word"]["revision"],
            "base_lifecycle_revision": archived["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["word"]["schema_version"], 3);
    assert_eq!(restored["word"]["kind"], "phrase");
    assert_eq!(restored["word"]["status"], "published");

    let (status, list) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?page=1&page_size=20&q=native%20phrase"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["words"][0]["kind"], "phrase");
    assert_eq!(
        list["words"][0]["presentation"]["matched_surfaces"],
        json!(["native phrase"])
    );
    assert_eq!(
        list["words"][0]["dialects"],
        json!(["uk", "us"]),
        "短语行同样按词性 spelling_mode 聚合方言摘要：{list}"
    );
}

fn resolved_component_json(
    target: &Value,
    target_publication_id: Uuid,
    dialect: &str,
    literal: &str,
) -> Value {
    let pos = &target["word"]["forms"]["pos"][0];
    let form = &pos["forms"][0];
    json!({
        "id": Uuid::now_v7(),
        "state": "resolved",
        "literal": literal,
        "target_word_id": target["word"]["id"],
        "target_publication_id": target_publication_id,
        "target_pos_id": pos["pos_id"],
        "target_base_form_id": form["id"],
        "target_sense_id": target["word"]["meanings"]["pos"][0]["senses"][0]["id"],
        "target_form_id": form["id"],
        "target_variant_id": form["regional_variants"][dialect]["id"],
        "target_dialect": dialect,
        "target_form_type": "base",
        "target_headword": target["word"]["presentation"]["label"],
        "target_gloss": target["word"]["meanings"]["pos"][0]["senses"][0]["definitions"][0]
            ["content"]["text"]
    })
}

/// 造一条短语草稿：探测、创建，再把完整词形 fixture 的拼写改成短语本身。
/// 返回 entry_id 与尚未保存的词形内容，成分与保存意图由调用方决定。
async fn create_v3_phrase_draft(state: &AppState, bearer: &str, surface: &str) -> (String, Value) {
    let (status, detection) = call(
        state,
        Method::POST,
        &format!("{ROOT}/detections"),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "phrase",
            "surface": surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let mut create_input = json!({
        "schema_version": 3,
        "detection_id": detection["detection_id"],
        "kind": "phrase"
    });
    if let Some(token) = detection["surface_match_page"]["surface_confirmation_token"].as_str() {
        create_input["confirmed_surface_match_token"] = json!(token);
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
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let entry_id = created["word"]["id"].as_str().unwrap().to_owned();
    let mut forms = complete_v3_forms_fixture();
    for form in forms["pos"][0]["forms"].as_array_mut().unwrap() {
        form["regional_variants"]["uk"]["spelling"] = json!(surface);
        form["regional_variants"]["us"]["spelling"] = json!(surface);
    }
    (entry_id, forms)
}

async fn create_published_v3_phrase(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
    surface: &str,
    uk_component_usages: Value,
) -> (Value, Uuid) {
    let (entry_id, mut forms) = create_v3_phrase_draft(state, bearer, surface).await;
    forms["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"] =
        uk_component_usages;
    let (_, forms_saved) =
        save_v3_forms_after_impact(state, bearer, &entry_id, 1, "complete", forms).await;
    let (status, meanings_saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": forms_saved["word"]["revision"],
            "intent": "complete",
            "content":
                complete_v3_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone())
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{meanings_saved}");
    let (status, published) = publish_ready_v3(state, bearer, &meanings_saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    let publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(&entry_id).unwrap())
            .fetch_one(pool)
            .await
            .unwrap();
    (published, publication_id)
}

#[sqlx::test]
async fn v3_phrase_components_may_target_phrases_with_cycle_and_depth_guards(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let word_draft =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["The harbour is calm."])
            .await;
    let (status, word_published) = publish_ready_v3(&state, &bearer, &word_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{word_published}");
    let word_entry_id = word_published["word"]["id"].as_str().unwrap();
    let word_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(word_entry_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();

    let (inner_phrase, inner_publication_id) = create_published_v3_phrase(
        &state,
        &pool,
        &bearer,
        "guard phrase",
        json!([resolved_component_json(
            &word_published,
            word_publication_id,
            "uk",
            "harbour"
        )]),
    )
    .await;
    let inner_entry_id = inner_phrase["word"]["id"].as_str().unwrap();

    let (status, resolved) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/sentence-targets/resolve"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "sentence_text": "guard phrase",
            "source_dialect": "common",
            "mode": "selected_segments",
            "selected_segments": [{"start": 0, "end": 12, "surface": "guard phrase"}],
            "include_drafts": false
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    let phrase_candidate = resolved["range_results"][0]["published_matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["entry_id"] == inner_entry_id)
        .expect("selected-segments resolve must surface the published phrase");
    assert_eq!(phrase_candidate["kind"], "phrase");
    let candidate_forms = phrase_candidate["forms"].as_array().unwrap();
    assert_eq!(
        candidate_forms.len(),
        4,
        "词形清单应覆盖该词性下全部词形变体：{phrase_candidate}"
    );
    assert!(
        candidate_forms
            .iter()
            .all(|form| form["spelling"] == "guard phrase" && form["form_type"] == "base")
    );
    assert!(
        candidate_forms
            .iter()
            .any(|form| form["variant_id"] == phrase_candidate["matched_variant_id"])
    );

    let (status, word_resolved) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/sentence-targets/resolve"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "sentence_text": "harbour",
            "source_dialect": "common",
            "mode": "selected_segments",
            "selected_segments": [{"start": 0, "end": 7, "surface": "harbour"}],
            "include_drafts": false
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{word_resolved}");
    let word_candidate = word_resolved["range_results"][0]["published_matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["entry_id"] == word_entry_id)
        .expect("selected-segments resolve must surface the published word");
    assert_eq!(word_candidate["kind"], "word");
    assert_eq!(word_candidate["forms"].as_array().unwrap().len(), 4);
    assert!(
        word_candidate["forms"]
            .as_array()
            .unwrap()
            .iter()
            .any(|form| form["spelling"] == "harbor" && form["dialect"] == "us")
    );

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "phrase",
            "surface": "double phrase"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let (status, outer_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "phrase"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{outer_created}");
    let outer_entry_id = outer_created["word"]["id"].as_str().unwrap();

    let mut cycle_forms = complete_v3_forms_fixture();
    for form in cycle_forms["pos"][0]["forms"].as_array_mut().unwrap() {
        form["regional_variants"]["uk"]["spelling"] = json!("double phrase");
        form["regional_variants"]["us"]["spelling"] = json!("double phrase");
    }
    cycle_forms["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"] = json!([{
        "id": Uuid::now_v7(),
        "state": "resolved",
        "literal": "double",
        "target_word_id": outer_entry_id,
        "target_publication_id": Uuid::now_v7(),
        "target_pos_id": Uuid::now_v7(),
        "target_base_form_id": Uuid::now_v7(),
        "target_sense_id": Uuid::now_v7(),
        "target_form_id": Uuid::now_v7(),
        "target_variant_id": Uuid::now_v7(),
        "target_dialect": "uk",
        "target_form_type": "base",
        "target_headword": "double phrase",
        "target_gloss": "自环"
    }]);
    let (status, cycle_rejected) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{outer_entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "complete",
            "content": cycle_forms
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{cycle_rejected}");
    assert_eq!(cycle_rejected["field"], "component_usages");
    assert!(
        cycle_rejected["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("phrase itself"),
        "{cycle_rejected}"
    );

    let mut outer_forms = complete_v3_forms_fixture();
    for form in outer_forms["pos"][0]["forms"].as_array_mut().unwrap() {
        form["regional_variants"]["uk"]["spelling"] = json!("double phrase");
        form["regional_variants"]["us"]["spelling"] = json!("double phrase");
    }
    outer_forms["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"] =
        json!([resolved_component_json(
            &inner_phrase,
            inner_publication_id,
            "uk",
            "guard phrase"
        )]);
    let (_, outer_forms_saved) =
        save_v3_forms_after_impact(&state, &bearer, outer_entry_id, 1, "complete", outer_forms)
            .await;
    assert_eq!(
        outer_forms_saved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"]
            [0]["target_word_id"],
        json!(inner_entry_id),
        "短语成分应能锚定已发布短语目标"
    );
    let (status, outer_meanings) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{outer_entry_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": outer_forms_saved["word"]["revision"],
            "intent": "complete",
            "content": complete_v3_meanings_fixture(
                outer_forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone()
            )
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outer_meanings}");
    let (status, outer_published) = publish_ready_v3(&state, &bearer, &outer_meanings).await;
    assert_eq!(status, StatusCode::CREATED, "{outer_published}");
    let outer_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(Uuid::parse_str(outer_entry_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();

    let (status, third_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "phrase",
            "surface": "third phrase"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{third_detection}");
    let (status, third_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": third_detection["detection_id"],
            "kind": "phrase"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{third_created}");
    let third_entry_id = third_created["word"]["id"].as_str().unwrap();
    let mut third_forms = complete_v3_forms_fixture();
    for form in third_forms["pos"][0]["forms"].as_array_mut().unwrap() {
        form["regional_variants"]["uk"]["spelling"] = json!("third phrase");
        form["regional_variants"]["us"]["spelling"] = json!("third phrase");
    }
    third_forms["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"] =
        json!([resolved_component_json(
            &outer_published,
            outer_publication_id,
            "uk",
            "double phrase"
        )]);
    let (status, depth_rejected) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{third_entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "complete",
            "content": third_forms
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{depth_rejected}");
    assert_eq!(depth_rejected["field"], "component_usages");
    assert!(
        depth_rejected["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("another phrase"),
        "{depth_rejected}"
    );
}

#[sqlx::test]
async fn v3_candidate_forms_carry_group_bases_for_cross_group_component_picks(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 目标词条改成两个同拼写原形各自成组、各带一条复数。候选按原形展开，所以「另一组的复数」
    // 落进候选清单时，配套的 base form 只能从词形自带的 base_form_ids 里取。
    let word_created = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let word_entry_id = word_created["word"]["id"].as_str().unwrap().to_owned();
    let mut forms = word_created["word"]["forms"].clone();
    let first_base_id = forms["pos"][0]["forms"][0]["id"].clone();
    let second_base_id = forms["pos"][0]["forms"][1]["id"].clone();
    let first_plural_id = Uuid::now_v7();
    let second_plural_id = Uuid::now_v7();
    let plural = |id: Uuid| {
        json!({
            "id": id,
            "form_type": "plural",
            "regional_variants": {
                "mode": "uk_us",
                "uk": {
                    "id": Uuid::now_v7(),
                    "dialect": "uk",
                    "spelling": "harbours",
                    "origin": "manual",
                    "pronunciations": [{
                        "id": Uuid::now_v7(),
                        "dict_phonetic": "/ˈhɑːbəz/",
                        "actual_pron": "hɑːbəz",
                        "style": "normal"
                    }]
                },
                "us": {
                    "id": Uuid::now_v7(),
                    "dialect": "us",
                    "spelling": "harbors",
                    "origin": "manual",
                    "pronunciations": [{
                        "id": Uuid::now_v7(),
                        "dict_phonetic": "/ˈhɑrbərz/",
                        "actual_pron": "hɑrbərz",
                        "style": "normal"
                    }]
                }
            }
        })
    };
    {
        let pos_forms = forms["pos"][0]["forms"].as_array_mut().unwrap();
        pos_forms.push(plural(first_plural_id));
        pos_forms.push(plural(second_plural_id));
    }
    forms["pos"][0]["form_groups"] = json!([{
        "id": Uuid::now_v7(),
        "is_regular": true,
        "members": [
            {"id": Uuid::now_v7(), "form_id": first_base_id},
            {"id": Uuid::now_v7(), "form_id": first_plural_id}
        ]
    }, {
        "id": Uuid::now_v7(),
        "is_regular": false,
        "members": [
            {"id": Uuid::now_v7(), "form_id": second_base_id},
            {"id": Uuid::now_v7(), "form_id": second_plural_id}
        ]
    }]);
    let (_, forms_saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        &word_entry_id,
        word_created["word"]["revision"].as_i64().unwrap(),
        "complete",
        forms,
    )
    .await;
    let meanings_saved = save_v3_meanings(
        &state,
        &bearer,
        &forms_saved,
        complete_v3_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone()),
    )
    .await;
    let (status, word_published) = publish_ready_v3(&state, &bearer, &meanings_saved).await;
    assert_eq!(status, StatusCode::CREATED, "{word_published}");

    let (status, resolved) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/sentence-targets/resolve"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "sentence_text": "harbour",
            "source_dialect": "common",
            "mode": "selected_segments",
            "selected_segments": [{"start": 0, "end": 7, "surface": "harbour"}],
            "include_drafts": false
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    let candidate = resolved["range_results"][0]["published_matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| {
            candidate["entry_id"] == json!(word_entry_id)
                && candidate["base_form_id"] == first_base_id
        })
        .expect("第一个原形应有自己的候选行");
    let cross_group_form = candidate["forms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|form| form["form_id"] == json!(second_plural_id) && form["dialect"] == "uk")
        .expect("候选词形清单应覆盖另一变化组的复数");
    assert_eq!(
        cross_group_form["base_form_ids"],
        json!([second_base_id]),
        "跨组词形要指回自己那组的原形：{candidate}"
    );

    let (phrase_entry_id, mut phrase_forms) =
        create_v3_phrase_draft(&state, &bearer, "harbour club").await;
    // 成分完全由 resolve 的候选载荷拼出，正是前端级联选择那一步手上的数据。
    let component = |base_form_id: &Value| {
        json!([{
            "id": Uuid::now_v7(),
            "state": "resolved",
            "literal": "harbours",
            "target_word_id": candidate["entry_id"],
            "target_publication_id": candidate["publication_id"],
            "target_pos_id": candidate["pos_id"],
            "target_base_form_id": base_form_id,
            "target_sense_id": candidate["senses"][0]["sense_id"],
            "target_form_id": cross_group_form["form_id"],
            "target_variant_id": cross_group_form["variant_id"],
            "target_dialect": cross_group_form["dialect"],
            "target_form_type": cross_group_form["form_type"],
            "target_headword": candidate["headword"],
            "target_gloss": candidate["senses"][0]["gloss"]
        }])
    };

    phrase_forms["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"] =
        component(&candidate["base_form_id"]);
    let (status, rejected) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{phrase_entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "complete",
            "content": phrase_forms.clone()
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "沿用候选行的 base_form_id 会让 form 与 base 跨组：{rejected}"
    );
    assert_eq!(rejected["field"], "component_usages");

    // 换成词形自带的 base_form_ids 后保存必须通过（helper 内断言 200），再回读一次
    // 确认跨组成分原样落库。
    phrase_forms["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"] =
        component(&cross_group_form["base_form_ids"][0]);
    let (_, phrase_saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        &phrase_entry_id,
        1,
        "complete",
        phrase_forms,
    )
    .await;
    assert_eq!(
        phrase_saved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"]
            [0]["target_base_form_id"],
        second_base_id,
        "跨组成分的 base 应原样回读：{phrase_saved}"
    );
}

/// 造一条词形已完成、首个 sense 带释义级成分的短语草稿。
async fn create_v3_phrase_with_sense_components(
    state: &AppState,
    bearer: &str,
    surface: &str,
    component_usages: Value,
) -> Value {
    let (entry_id, forms) = create_v3_phrase_draft(state, bearer, surface).await;
    let (_, forms_saved) =
        save_v3_forms_after_impact(state, bearer, &entry_id, 1, "complete", forms).await;
    let mut meanings =
        complete_v3_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone());
    meanings["pos"][0]["senses"][0]["component_usages"] = component_usages;
    save_v3_meanings(state, bearer, &forms_saved, meanings).await
}

async fn current_publication_id(pool: &PgPool, entry_id: Uuid) -> Uuid {
    sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
        .bind(entry_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn sense_component_rows(pool: &PgPool, entry_id: Uuid) -> Vec<(Uuid, Uuid, i16, String)> {
    sqlx::query_as(
        r#"
        SELECT id, sense_id, ordinal, literal
        FROM lexicon.v3_phrase_sense_component_usages
        WHERE entry_id = $1
        ORDER BY sense_id, ordinal
        "#,
    )
    .bind(entry_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn v3_sense_phrase_components_persist_publish_and_survive_forms_resave(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_draft =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["The harbour is calm."])
            .await;
    let (status, target_published) = publish_ready_v3(&state, &bearer, &target_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{target_published}");
    let target_entry_uuid =
        Uuid::parse_str(target_published["word"]["id"].as_str().unwrap()).unwrap();
    let target_publication_id = current_publication_id(&pool, target_entry_uuid).await;
    let target_sense_uuid = Uuid::parse_str(
        target_published["word"]["meanings"]["pos"][0]["senses"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();

    let component =
        resolved_component_json(&target_published, target_publication_id, "uk", "native");
    let component_id = Uuid::parse_str(component["id"].as_str().unwrap()).unwrap();
    let saved = create_v3_phrase_with_sense_components(
        &state,
        &bearer,
        "native phrase",
        json!([component.clone()]),
    )
    .await;
    let entry_id = saved["word"]["id"].as_str().unwrap().to_owned();
    let entry_uuid = Uuid::parse_str(&entry_id).unwrap();
    let sense_uuid = Uuid::parse_str(
        saved["word"]["meanings"]["pos"][0]["senses"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        saved["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"][0]["literal"],
        "native"
    );
    assert_eq!(
        saved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["component_usages"],
        json!([]),
        "释义级成分不得回流到词形变体：{saved}"
    );
    assert_eq!(
        sense_component_rows(&pool, entry_uuid).await,
        vec![(component_id, sense_uuid, 0, "native".to_owned())],
        "成分行必须以 sense 为 owner 落进释义级表"
    );
    let node: (String, String, Option<Uuid>, bool) = sqlx::query_as(
        r#"
        SELECT node_type, node_role, parent_node_id, removed_from_draft_at IS NULL
        FROM lexicon.nodes WHERE id = $1 AND entry_id = $2
        "#,
    )
    .bind(component_id)
    .bind(entry_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        node,
        (
            "phrase_component_usage".to_owned(),
            "meanings.phrase_component_usage".to_owned(),
            Some(sense_uuid),
            true
        )
    );

    let (status, reloaded) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reloaded}");
    assert_eq!(
        reloaded["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"][0]["id"],
        component["id"],
        "GET 必须回显释义级成分"
    );

    let mut without_key = reloaded["word"]["meanings"].clone();
    without_key["pos"][0]["senses"][0]
        .as_object_mut()
        .unwrap()
        .remove("component_usages");
    let preserved = save_v3_meanings(&state, &bearer, &reloaded, without_key).await;
    assert_eq!(
        preserved["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"][0]["id"],
        component["id"],
        "缺键的旧客户端不得清空释义级成分"
    );

    let mut cleared_content = preserved["word"]["meanings"].clone();
    cleared_content["pos"][0]["senses"][0]["component_usages"] = json!([]);
    let cleared = save_v3_meanings(&state, &bearer, &preserved, cleared_content).await;
    assert!(
        cleared["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"].is_null(),
        "空成分列表序列化时省略：{cleared}"
    );
    assert!(
        sense_component_rows(&pool, entry_uuid).await.is_empty(),
        "显式空数组必须清空成分行"
    );
    let retired: bool = sqlx::query_scalar(
        "SELECT removed_from_draft_at IS NOT NULL FROM lexicon.nodes WHERE id = $1",
    )
    .bind(component_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(retired, "清空后成分节点必须退役");

    let mut restored_content = cleared["word"]["meanings"].clone();
    restored_content["pos"][0]["senses"][0]["component_usages"] = json!([component.clone()]);
    let restored = save_v3_meanings(&state, &bearer, &cleared, restored_content).await;
    assert_eq!(
        sense_component_rows(&pool, entry_uuid).await.len(),
        1,
        "同 id 重新勾选必须能复活成分节点"
    );

    let (_, forms_resaved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        &entry_id,
        restored["word"]["revision"].as_i64().unwrap(),
        "complete",
        restored["word"]["forms"].clone(),
    )
    .await;
    assert_eq!(
        forms_resaved["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"][0]["id"],
        component["id"],
        "词形步重存不得丢掉释义级成分：{forms_resaved}"
    );
    assert_eq!(
        sense_component_rows(&pool, entry_uuid).await.len(),
        1,
        "词形步重存后成分行必须原样重建"
    );

    let (status, published) = publish_ready_v3(&state, &bearer, &forms_resaved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_eq!(
        published["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"][0]["id"],
        component["id"],
        "发布响应必须仍带释义级成分：{published}"
    );
    let publication_id = current_publication_id(&pool, entry_uuid).await;
    let snapshot_components: Value = sqlx::query_scalar(
        "SELECT snapshot->'meanings'->'pos'->0->'senses'->0->'component_usages' FROM lexicon.entry_publications WHERE id = $1",
    )
    .bind(publication_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        snapshot_components[0]["id"], component["id"],
        "发布快照必须固化释义级成分，否则下游只能读到空"
    );
    let refs: Vec<(Uuid, Uuid)> = sqlx::query_as(
        r#"
        SELECT source_node_id, target_sense_id
        FROM lexicon.entry_publication_sense_refs
        WHERE publication_id = $1 AND reference_kind = 'phrase_component'
        "#,
    )
    .bind(publication_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(refs, vec![(component_id, target_sense_uuid)]);
    let published_component_nodes: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*) FROM lexicon.entry_publication_nodes
        WHERE publication_id = $1 AND node_type = 'phrase_component_usage'
        "#,
    )
    .bind(publication_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(published_component_nodes, 1);

    let (status, discovered) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/sentence-targets/resolve"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "sentence_text": "A native phrase appears.",
            "source_dialect": "common",
            "mode": "all_published_targets",
            "page_size_per_range": 20
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{discovered}");
    let phrase_candidate = discovered["range_results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|range| range["published_matches"].as_array().unwrap())
        .find(|candidate| candidate["entry_id"] == entry_id)
        .expect("discovery must return the published phrase");
    assert_eq!(
        phrase_candidate["component_usages"],
        json!([]),
        "候选级语义不变：仍是命中词形的成分，本例词形没有成分"
    );
    assert_eq!(
        phrase_candidate["senses"][0]["component_usages"][0]["id"], component["id"],
        "候选的 sense 行必须带出释义级成分：{phrase_candidate}"
    );
}

#[sqlx::test]
async fn v3_publish_with_newly_bound_relations_keeps_sense_phrase_components(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_draft =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["The harbour is calm."])
            .await;
    let (status, target_published) = publish_ready_v3(&state, &bearer, &target_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{target_published}");
    let target_entry_uuid =
        Uuid::parse_str(target_published["word"]["id"].as_str().unwrap()).unwrap();
    let target_publication_id = current_publication_id(&pool, target_entry_uuid).await;

    let component =
        resolved_component_json(&target_published, target_publication_id, "uk", "bound");
    let component_id = Uuid::parse_str(component["id"].as_str().unwrap()).unwrap();
    let saved = create_v3_phrase_with_sense_components(
        &state,
        &bearer,
        "bound phrase",
        json!([component.clone()]),
    )
    .await;
    let entry_uuid = Uuid::parse_str(saved["word"]["id"].as_str().unwrap()).unwrap();

    // 待建关联词让发布走 newly_bound 分支：sync_canonical_meanings 会再退役一轮成分节点。
    let pending_headword = format!("boundpending{}", admin_id.simple());
    let mut meanings = saved["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "pending_target_headword": pending_headword,
        "score": "88.00"
    }]);
    let with_pending = save_v3_meanings(&state, &bearer, &saved, meanings).await;

    let (status, published) = publish_ready_v3(&state, &bearer, &with_pending).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "带 newly_bound 的发布必须成功：{published}"
    );
    assert_eq!(
        published["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"][0]["id"],
        component["id"],
        "newly_bound 路径的 V2 往返不得吞掉释义级成分：{published}"
    );
    assert!(
        published["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_word_id"]
            .is_string(),
        "前置：这条发布必须真的物化了待建关联词"
    );

    let publication_id = current_publication_id(&pool, entry_uuid).await;
    let published_component_nodes: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*) FROM lexicon.entry_publication_nodes
        WHERE publication_id = $1 AND node_type = 'phrase_component_usage'
        "#,
    )
    .bind(publication_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        published_component_nodes, 1,
        "sync_canonical_meanings 之后必须重建成分节点，否则发布节点会漏行"
    );
    let refs: Vec<Uuid> = sqlx::query_scalar(
        r#"
        SELECT source_node_id FROM lexicon.entry_publication_sense_refs
        WHERE publication_id = $1 AND reference_kind = 'phrase_component'
        "#,
    )
    .bind(publication_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(refs, vec![component_id]);
    assert_eq!(
        sense_component_rows(&pool, entry_uuid).await.len(),
        1,
        "发布后草稿侧成分行必须还在"
    );
    let projected: Value = sqlx::query_scalar(
        "SELECT meanings->'pos'->0->'senses'->0->'component_usages' FROM lexicon.entry_editor_projection WHERE entry_id = $1",
    )
    .bind(entry_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        projected[0]["id"], component["id"],
        "sync_canonical_meanings 覆盖投影时必须回填释义级成分"
    );
}

fn sense_component_issue<'a>(body: &'a Value, code: &str) -> &'a Value {
    body["field_issues"]
        .as_array()
        .unwrap_or_else(|| panic!("必须返回 node 级 issue：{body}"))
        .iter()
        .find(|issue| issue["code"] == code)
        .unwrap_or_else(|| panic!("缺少 {code}：{body}"))
}

#[sqlx::test]
async fn v3_sense_phrase_component_issues_cover_the_closed_code_catalog(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let word_draft =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["The harbour is calm."])
            .await;
    let (status, word_published) = publish_ready_v3(&state, &bearer, &word_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{word_published}");
    let word_entry_id = word_published["word"]["id"].as_str().unwrap().to_owned();
    let word_publication_id =
        current_publication_id(&pool, Uuid::parse_str(&word_entry_id).unwrap()).await;

    // phrase_component_not_allowed：非短语词条不得携带成分。
    let mut word_meanings = writable_v3_meanings(&word_published);
    word_meanings["pos"][0]["senses"][0]["component_usages"] = json!([{
        "id": Uuid::now_v7(),
        "state": "unresolved",
        "literal": "harbour"
    }]);
    let (status, not_allowed) = save_v3_meanings_raw(
        &state,
        &bearer,
        &word_entry_id,
        word_published["word"]["revision"].as_i64().unwrap(),
        word_meanings,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{not_allowed}");
    let issue = sense_component_issue(&not_allowed, "phrase_component_not_allowed");
    assert_eq!(issue["step"], "meanings");
    assert_eq!(issue["field"], "component_usages");
    assert_eq!(
        issue["node_id"],
        word_published["word"]["meanings"]["pos"][0]["senses"][0]["id"]
    );
    assert_eq!(issue["node_location"]["node_role"], "meanings.sense");
    assert_eq!(
        issue["node_location"]["ancestor_node_ids"],
        json!([word_published["word"]["meanings"]["pos"][0]["pos_id"]])
    );

    // 套娃守卫的两个目标：成分挂在词形上的（存量口径）与挂在释义上的（新口径），
    // 都要能被「目标短语自身含成分」查出来。
    let (inner_phrase, inner_publication_id) = create_published_v3_phrase(
        &state,
        &pool,
        &bearer,
        "inner guard phrase",
        json!([resolved_component_json(
            &word_published,
            word_publication_id,
            "uk",
            "inner"
        )]),
    )
    .await;
    let (forms_nested_phrase, forms_nested_publication_id) = create_published_v3_phrase(
        &state,
        &pool,
        &bearer,
        "forms nested phrase",
        json!([resolved_component_json(
            &inner_phrase,
            inner_publication_id,
            "uk",
            "inner"
        )]),
    )
    .await;
    let sense_nested_draft = create_v3_phrase_with_sense_components(
        &state,
        &bearer,
        "sense nested phrase",
        json!([resolved_component_json(
            &inner_phrase,
            inner_publication_id,
            "uk",
            "inner"
        )]),
    )
    .await;
    let (status, sense_nested_phrase) =
        publish_ready_v3(&state, &bearer, &sense_nested_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{sense_nested_phrase}");
    let sense_nested_publication_id = current_publication_id(
        &pool,
        Uuid::parse_str(sense_nested_phrase["word"]["id"].as_str().unwrap()).unwrap(),
    )
    .await;

    let (victim_entry_id, victim_forms) =
        create_v3_phrase_draft(&state, &bearer, "issue phrase").await;
    let (_, victim_forms_saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        &victim_entry_id,
        1,
        "complete",
        victim_forms,
    )
    .await;
    let victim_revision = victim_forms_saved["word"]["revision"].as_i64().unwrap();
    let victim_pos_id = victim_forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone();
    let base_meanings = complete_v3_meanings_fixture(victim_pos_id.clone());
    let victim_sense_id = base_meanings["pos"][0]["senses"][0]["id"].clone();
    let with_components = |usages: Value| {
        let mut meanings = base_meanings.clone();
        meanings["pos"][0]["senses"][0]["component_usages"] = usages;
        meanings
    };
    let reject = async |usages: Value| {
        let (status, body) = save_v3_meanings_raw(
            &state,
            &bearer,
            &victim_entry_id,
            victim_revision,
            with_components(usages),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        body
    };

    // phrase_component_limit_exceeded
    let too_many = (0..101)
        .map(|index| {
            json!({
                "id": Uuid::now_v7(),
                "state": "unresolved",
                "literal": format!("token{index}")
            })
        })
        .collect::<Vec<_>>();
    let limit = reject(json!(too_many)).await;
    let issue = sense_component_issue(&limit, "phrase_component_limit_exceeded");
    assert_eq!(issue["node_id"], victim_sense_id);
    assert_eq!(issue["field"], "component_usages");

    // phrase_component_literal_invalid
    let bad_literal_id = Uuid::now_v7();
    let literal_invalid = reject(json!([{
        "id": bad_literal_id,
        "state": "unresolved",
        "literal": " issue"
    }]))
    .await;
    let issue = sense_component_issue(&literal_invalid, "phrase_component_literal_invalid");
    assert_eq!(issue["node_id"], bad_literal_id.to_string());
    assert_eq!(issue["field"], "literal");
    assert_eq!(
        issue["node_location"]["node_role"],
        "meanings.phrase_component_usage"
    );
    assert_eq!(
        issue["node_location"]["ancestor_node_ids"],
        json!([victim_pos_id, victim_sense_id])
    );

    // phrase_component_self_target
    let mut self_target =
        resolved_component_json(&word_published, word_publication_id, "uk", "issue");
    self_target["target_word_id"] = json!(victim_entry_id);
    let self_rejected = reject(json!([self_target])).await;
    let issue = sense_component_issue(&self_rejected, "phrase_component_self_target");
    assert_eq!(issue["field"], "target");

    // phrase_component_target_unavailable
    let mut unavailable =
        resolved_component_json(&word_published, word_publication_id, "uk", "issue");
    unavailable["target_publication_id"] = json!(Uuid::now_v7());
    let unavailable_rejected = reject(json!([unavailable])).await;
    assert!(has_issue(
        &unavailable_rejected,
        "phrase_component_target_unavailable"
    ));

    // phrase_component_target_stale
    let mut stale = resolved_component_json(&word_published, word_publication_id, "uk", "issue");
    stale["target_gloss"] = json!("对不上的词义");
    let stale_rejected = reject(json!([stale])).await;
    assert!(has_issue(&stale_rejected, "phrase_component_target_stale"));

    // phrase_component_target_nested：目标短语的成分挂在词形侧
    let forms_nested = reject(json!([resolved_component_json(
        &forms_nested_phrase,
        forms_nested_publication_id,
        "uk",
        "issue"
    )]))
    .await;
    assert!(
        has_issue(&forms_nested, "phrase_component_target_nested"),
        "存量短语的成分还在 forms 上，套娃检测必须扫得到：{forms_nested}"
    );

    // phrase_component_target_nested：目标短语的成分挂在释义侧
    let sense_nested = reject(json!([resolved_component_json(
        &sense_nested_phrase,
        sense_nested_publication_id,
        "uk",
        "issue"
    )]))
    .await;
    assert!(
        has_issue(&sense_nested, "phrase_component_target_nested"),
        "释义级成分同样要参与套娃检测：{sense_nested}"
    );

    // 只套一层是允许的：目标短语的成分指向单词。
    let (status, accepted) = save_v3_meanings_raw(
        &state,
        &bearer,
        &victim_entry_id,
        victim_revision,
        with_components(json!([resolved_component_json(
            &inner_phrase,
            inner_publication_id,
            "uk",
            "issue"
        )])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "短语→短语→单词必须放行：{accepted}");
}

#[sqlx::test]
async fn v3_sense_phrase_component_refs_guard_target_sense_removal_and_restore(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 目标词准备两条词义，才谈得上「删掉被引用的那条」。
    let word_draft =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["The harbour is calm."])
            .await;
    let mut two_senses = writable_v3_meanings(&word_draft);
    let spare_sense = json!({
        "id": Uuid::now_v7(),
        "sub_pos": "N-COUNT",
        "level": "A1",
        "sense_group_id": two_senses["sense_groups"][0]["id"],
        "frequency": "100",
        "depends_on_context": false,
        "definitions": [{
            "definition_mode": "zh_definition",
            "id": Uuid::now_v7(),
            "content_id": Uuid::now_v7(),
            "level": "A1",
            "grammar_structure_id": two_senses["pos"][0]["grammar_structures"][0]["id"],
            "content": rich_text("备用词义")
        }],
        "sentences": [],
        "relations": []
    });
    two_senses["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .push(spare_sense);
    let word_saved = save_v3_meanings(&state, &bearer, &word_draft, two_senses).await;
    let (status, word_published) = publish_ready_v3(&state, &bearer, &word_saved).await;
    assert_eq!(status, StatusCode::CREATED, "{word_published}");
    let word_entry_id = word_published["word"]["id"].as_str().unwrap().to_owned();
    let word_entry_uuid = Uuid::parse_str(&word_entry_id).unwrap();
    let word_publication_id = current_publication_id(&pool, word_entry_uuid).await;
    let referenced_sense_id =
        word_published["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();

    let component = resolved_component_json(&word_published, word_publication_id, "uk", "guarded");
    let component_id = Uuid::parse_str(component["id"].as_str().unwrap()).unwrap();
    let phrase_draft = create_v3_phrase_with_sense_components(
        &state,
        &bearer,
        "guarded phrase",
        json!([component]),
    )
    .await;
    let (status, phrase_published) = publish_ready_v3(&state, &bearer, &phrase_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{phrase_published}");
    let phrase_entry_id = phrase_published["word"]["id"].as_str().unwrap().to_owned();

    // 目标词草稿里删掉被引用的词义：草稿放行，发布时 fail closed。
    let mut without_referenced_sense = writable_v3_meanings(&word_published);
    let spare = without_referenced_sense["pos"][0]["senses"][1].clone();
    without_referenced_sense["pos"][0]["senses"] = json!([spare]);
    let word_pruned =
        save_v3_meanings(&state, &bearer, &word_published, without_referenced_sense).await;
    let (status, blocked) = publish_ready_v3(&state, &bearer, &word_pruned).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "释义级成分引用的词义不得被删掉后发布：{blocked}"
    );
    let issue = sense_component_issue(&blocked, "sense_has_inbound_publication_refs");
    assert_eq!(issue["node_id"], referenced_sense_id);
    // V3 的 issue 形状不带 reference_location，来源只能从发布引用表核对。
    let blocking_refs: Vec<(Uuid, String)> = sqlx::query_as(
        r#"
        SELECT source_node_id, reference_kind
        FROM lexicon.entry_publication_sense_refs
        WHERE target_entry_id = $1 AND target_sense_id = $2
        "#,
    )
    .bind(word_entry_uuid)
    .bind(Uuid::parse_str(referenced_sense_id.as_str().unwrap()).unwrap())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        blocking_refs,
        vec![(component_id, "phrase_component".to_owned())]
    );

    // 归档短语解除入站守卫，目标词才能把那条词义发布掉。
    let (status, phrase_archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{phrase_entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": phrase_published["word"]["revision"],
            "base_lifecycle_revision": phrase_published["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{phrase_archived}");
    let (status, word_republished) = publish_ready_v3(&state, &bearer, &word_pruned).await;
    assert_eq!(status, StatusCode::CREATED, "{word_republished}");

    // 恢复短语时它的当前发布仍指着一条已消失的词义——出站守卫必须认得成分引用。
    let (status, restore_blocked) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{phrase_entry_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": phrase_archived["word"]["revision"],
            "base_lifecycle_revision": phrase_archived["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "成分目标词义已消失时不得恢复：{restore_blocked}"
    );
    assert_eq!(
        restore_blocked["code"],
        "entry_has_unavailable_publication_refs"
    );
    assert!(
        restore_blocked["meta"]["reference_locations"]
            .as_array()
            .is_some_and(|locations| locations
                .iter()
                .any(|location| location["reference_kind"] == "phrase_component"
                    && location["source_node_id"] == component_id.to_string())),
        "出站守卫必须点名那条成分引用：{restore_blocked}"
    );
}

/// 释义级绑定的立身之本：同一短语的不同释义各带各的成分，互不串味，
/// 而且例句关联固化的是**被选中的那一条** sense 的成分。
#[sqlx::test]
async fn v3_sense_phrase_components_are_bound_per_sense_not_shared(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let target_draft =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["The harbour is calm."])
            .await;
    let (status, target_published) = publish_ready_v3(&state, &bearer, &target_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{target_published}");
    let target_publication_id = current_publication_id(
        &pool,
        Uuid::parse_str(target_published["word"]["id"].as_str().unwrap()).unwrap(),
    )
    .await;

    let (entry_id, forms) = create_v3_phrase_draft(&state, &bearer, "split phrase").await;
    let (_, forms_saved) =
        save_v3_forms_after_impact(&state, &bearer, &entry_id, 1, "complete", forms).await;
    let entry_uuid = Uuid::parse_str(&entry_id).unwrap();
    let mut meanings =
        complete_v3_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone());

    let first_component =
        resolved_component_json(&target_published, target_publication_id, "uk", "split");
    let second_component =
        resolved_component_json(&target_published, target_publication_id, "uk", "phrase");
    let first_component_id = Uuid::parse_str(first_component["id"].as_str().unwrap()).unwrap();
    let second_component_id = Uuid::parse_str(second_component["id"].as_str().unwrap()).unwrap();
    meanings["pos"][0]["senses"][0]["component_usages"] = json!([first_component]);

    let second_sense_id = Uuid::now_v7();
    let second_sense = json!({
        "id": second_sense_id,
        "sub_pos": "N-COUNT",
        "level": "A1",
        "sense_group_id": meanings["sense_groups"][0]["id"],
        "frequency": "100",
        "depends_on_context": false,
        "definitions": [{
            "definition_mode": "zh_definition",
            "id": Uuid::now_v7(),
            "content_id": Uuid::now_v7(),
            "level": "A1",
            "grammar_structure_id": meanings["pos"][0]["grammar_structures"][0]["id"],
            "content": rich_text("第二个词义")
        }],
        "sentences": [],
        "relations": [],
        "component_usages": [second_component]
    });
    meanings["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .push(second_sense);

    let saved = save_v3_meanings(&state, &bearer, &forms_saved, meanings).await;
    let first_sense_uuid = Uuid::parse_str(
        saved["word"]["meanings"]["pos"][0]["senses"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        saved["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"][0]["literal"],
        "split"
    );
    assert_eq!(
        saved["word"]["meanings"]["pos"][0]["senses"][1]["component_usages"][0]["literal"],
        "phrase",
        "第二条释义必须拿到自己的成分，而不是第一条的：{saved}"
    );
    // 两边按同一个键排序再比：SQL 侧是 ORDER BY sense_id, ordinal，期望值也照此排，
    // 否则断言的通过与否会取决于 uuid 的生成顺序。
    let mut expected_rows = vec![
        (
            first_component_id,
            first_sense_uuid,
            0i16,
            "split".to_owned(),
        ),
        (
            second_component_id,
            second_sense_id,
            0i16,
            "phrase".to_owned(),
        ),
    ];
    expected_rows.sort_by_key(|(_, sense_id, ordinal, _)| (*sense_id, *ordinal));
    assert_eq!(
        sense_component_rows(&pool, entry_uuid).await,
        expected_rows,
        "两条成分行必须各挂各的 sense"
    );

    let (status, published) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");

    let (status, discovered) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/sentence-targets/resolve"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "sentence_text": "A split phrase appears.",
            "source_dialect": "common",
            "mode": "all_published_targets",
            "page_size_per_range": 20
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{discovered}");
    let phrase_candidate = discovered["range_results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|range| range["published_matches"].as_array().unwrap())
        .find(|candidate| candidate["entry_id"] == entry_id)
        .expect("discovery must return the published phrase");
    let senses = phrase_candidate["senses"].as_array().unwrap();
    assert_eq!(senses.len(), 2, "{phrase_candidate}");
    assert_eq!(senses[0]["component_usages"][0]["literal"], "split");
    assert_eq!(
        senses[1]["component_usages"][0]["literal"], "phrase",
        "候选的每条 sense 必须带自己的成分：{phrase_candidate}"
    );
}

#[sqlx::test]
async fn v3_sense_component_capability_is_always_on(pool: PgPool) {
    // 开关已移除：能力位恒为 true 并继续下发，写入非空成分不再被 503 拦。
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let (entry_id, forms) = create_v3_phrase_draft(&state, &bearer, "capability phrase").await;
    let (_, forms_saved) =
        save_v3_forms_after_impact(&state, &bearer, &entry_id, 1, "complete", forms).await;
    let capabilities = &forms_saved["word"]["capabilities"];
    assert_eq!(
        capabilities["sense_component_usages"], true,
        "能力位恒开且键必须在场，按能力位判断的客户端才不必跟着后端同批部署：{capabilities}"
    );
    assert_eq!(
        capabilities["draft_relation_prebinding"], true,
        "既有能力位仍旧无条件输出"
    );

    let mut meanings =
        complete_v3_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone());
    meanings["pos"][0]["senses"][0]["component_usages"] = json!([{
        "id": Uuid::now_v7(),
        "state": "unresolved",
        "literal": "capability"
    }]);
    let (status, accepted) = save_v3_meanings_raw(
        &state,
        &bearer,
        &entry_id,
        forms_saved["word"]["revision"].as_i64().unwrap(),
        meanings,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "写入闸已随开关一并移除，非空成分应当直接落库：{accepted}"
    );

    let (status, reread) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reread}");
    assert_eq!(
        reread["word"]["capabilities"]["sense_component_usages"],
        true
    );
}

#[sqlx::test]
async fn v3_phrase_with_sense_components_can_still_be_hard_deleted(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let word_draft =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["The harbour is calm."])
            .await;
    let (status, word_published) = publish_ready_v3(&state, &bearer, &word_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{word_published}");
    let word_publication_id = current_publication_id(
        &pool,
        Uuid::parse_str(word_published["word"]["id"].as_str().unwrap()).unwrap(),
    )
    .await;

    let phrase = create_v3_phrase_with_sense_components(
        &state,
        &bearer,
        "deletable phrase",
        json!([resolved_component_json(
            &word_published,
            word_publication_id,
            "uk",
            "deletable"
        )]),
    )
    .await;
    let phrase_entry_id = phrase["word"]["id"].as_str().unwrap().to_owned();
    let phrase_uuid = Uuid::parse_str(&phrase_entry_id).unwrap();
    assert_eq!(sense_component_rows(&pool, phrase_uuid).await.len(), 1);

    // 成分行对 lexicon.nodes 是 ON DELETE RESTRICT，硬删词条时必须仍能整条清掉。
    let (status, deleted) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{phrase_entry_id}"),
        &bearer,
        None,
        Some(json!({
            "base_revision": phrase["word"]["revision"],
            "base_lifecycle_revision": phrase["word"]["lifecycle_revision"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "未发布短语必须可硬删：{deleted}"
    );
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.v3_phrase_sense_component_usages WHERE entry_id = $1",
    )
    .bind(phrase_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, 0, "词条删除后不得残留成分行");
}

#[sqlx::test]
async fn legacy_surface_backfill_ignores_native_v3_entries(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let created = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap();
    let (status, empty_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "native-empty-shell"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{empty_detection}");
    assert_eq!(empty_detection["builtin_dictionary"]["status"], "not_found");
    let (status, empty_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": empty_detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{empty_created}");
    let surfaces_before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.surface_sources WHERE entry_id = $1 AND content_schema_version = 3 AND is_deleted = FALSE",
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(surfaces_before > 0);
    sqlx::query(
        r#"
        WITH offset_value AS (
            SELECT nextval('lexicon.surface_projection_event_offset_seq') AS value
        )
        INSERT INTO platform.outbox_events (
            id, aggregate_type, aggregate_id, aggregate_revision,
            event_type, payload, occurred_at, available_at
        )
        SELECT $1, 'lexicon.surface_projection', $2, value,
               'lexicon.surface_projection_replaced',
               jsonb_build_object(
                   'content_scope', 'draft',
                   'event_offset', value,
                   'source_revision', 1,
                   'source_count', 0
               ),
               now(), now()
        FROM offset_value
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();

    let backfill = run_surface_backfill(&pool).await.unwrap();
    assert_eq!(backfill.scanned_entries, 0, "legacy backfill 只能扫描 V2");
    assert!(backfill.parity.ready, "原生 V3 不得阻断 legacy parity");
    let surfaces_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.surface_sources WHERE entry_id = $1 AND content_schema_version = 3 AND is_deleted = FALSE",
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        surfaces_after, surfaces_before,
        "backfill 不得改写原生 V3 surface"
    );
}

#[sqlx::test]
async fn v3_detection_and_create_acknowledge_legacy_v2_surface_matches(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let legacy = create_ready_draft(&state, &pool, &bearer, "legacy-only-surface").await;
    let legacy_id = legacy["word"]["id"].clone();

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "legacy-only-surface"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    assert_eq!(detection["requires_acknowledgement"], true);
    assert_eq!(detection["matches"][0]["match_kind"], "legacy_v2");
    assert_eq!(detection["matches"][0]["match"]["source_schema_version"], 2);
    assert_eq!(
        detection["matches"][0]["match"]["existing"]["word_id"],
        legacy_id
    );
    let create = json!({
        "schema_version": 3,
        "detection_id": detection["detection_id"],
        "kind": "word"
    });
    let (status, _, required) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        create.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{required}");
    assert_eq!(
        required["meta"]["surface_match_page"]["items"][0]["match_kind"],
        "legacy_v2"
    );
    let mut confirmed = create;
    confirmed["confirmed_surface_match_token"] =
        required["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(confirmed),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    let entry_id = created["word"]["id"].as_str().unwrap();
    let mut forms = complete_v3_forms_fixture();
    forms["pos"][0]["forms"][0]["regional_variants"]["uk"]["spelling"] =
        json!("legacy-only-surface");
    forms["pos"][0]["forms"][0]["regional_variants"]["us"]["spelling"] =
        json!("legacy-only-surface");
    forms["pos"][0]["forms"][1]["regional_variants"]["uk"]["spelling"] =
        json!("legacy-only-surface");
    forms["pos"][0]["forms"][1]["regional_variants"]["us"]["spelling"] =
        json!("legacy-only-surface");
    let forms_body = json!({
        "schema_version": 3,
        "base_revision": 1,
        "intent": "complete",
        "content": forms
    });
    let (status, impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "content": forms
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(
        impact["surface_match_page"]["items"][0]["match_kind"],
        "legacy_v2"
    );

    let (status, _, forms_required) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        forms_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{forms_required}");
    assert_eq!(
        forms_required["meta"]["surface_match_page"]["items"][0]["match_kind"],
        "legacy_v2"
    );
    let mut confirmed_forms = forms_body;
    confirmed_forms["confirmed_surface_match_token"] =
        forms_required["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
    if let Some(token) = impact["confirmation_token"].as_str() {
        confirmed_forms["confirmed_impact_token"] = json!(token);
    }
    if let Some(token) = impact["surface_match_page"]["impact_confirmation_token"].as_str() {
        confirmed_forms["confirmed_impact_token"] = json!(token);
    }
    if let Some(token) =
        forms_required["meta"]["surface_match_page"]["impact_confirmation_token"].as_str()
    {
        confirmed_forms["confirmed_impact_token"] = json!(token);
    }
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(confirmed_forms),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let (status, mixed_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "legacy-only-surface"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{mixed_detection}");
    for collection in [
        mixed_detection["matches"].as_array().unwrap(),
        mixed_detection["surface_match_page"]["items"]
            .as_array()
            .unwrap(),
    ] {
        assert!(
            collection
                .iter()
                .any(|item| item["match_kind"] == "legacy_v2")
        );
        assert!(
            collection
                .iter()
                .any(|item| item["match_kind"] == "form_variant_v3")
        );
    }
    let (status, mixed_acknowledged) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": mixed_detection["detection_id"],
            "kind": "word",
            "confirmed_surface_match_token": mixed_detection["surface_match_page"]
                ["surface_confirmation_token"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{mixed_acknowledged}");
}

#[sqlx::test]
async fn v3_surface_warning_tokens_bind_actor_command_revision_digest_and_policy(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let other_admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let other_bearer = token(&state, other_admin_id);
    seed_dictionary_word(&pool, "harbour").await;
    seed_dictionary_word(&pool, "dockyard").await;

    let (status, first_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "harbour"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first_detection}");
    assert_eq!(first_detection["requires_acknowledgement"], false);
    let (status, first_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": first_detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first_created}");
    let first_entry_id = first_created["word"]["id"].as_str().unwrap();
    let first_forms = complete_v3_forms_fixture();
    let (status, first_impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{first_entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "content": first_forms.clone()
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first_impact}");
    let mut first_forms_input = json!({
        "schema_version": 3,
        "base_revision": 1,
        "intent": "complete",
        "content": first_forms
    });
    if let Some(token) = first_impact["confirmation_token"].as_str() {
        first_forms_input["confirmed_impact_token"] = json!(token);
    }
    let (status, first_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{first_entry_id}/steps/forms"),
        &bearer,
        None,
        Some(first_forms_input),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first_saved}");

    // Legacy V2 clients must see the native V3 form surface through the V2
    // compatibility view instead of receiving a 500 or silently missing it.
    let (status, legacy_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"language": "en", "headword": "harbour"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{legacy_detection}");
    assert_eq!(
        legacy_detection["smart_dictionary"]["status"], "warning",
        "V2 detection must surface native V3 candidates"
    );
    assert_eq!(
        legacy_detection["smart_dictionary"]["surface_match_page"]["schema_version"],
        2
    );

    let (status, duplicate_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "harbour"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{duplicate_detection}");
    assert_eq!(duplicate_detection["requires_acknowledgement"], true);
    assert_eq!(
        duplicate_detection["surface_match_page"]["schema_version"],
        3
    );
    assert_eq!(
        duplicate_detection["surface_match_page"]["items"][0]["match_kind"],
        "form_variant_v3"
    );
    assert_eq!(
        duplicate_detection["surface_match_page"]["items"][0]["match"]["entry_id"],
        first_entry_id
    );
    let duplicate_create = json!({
        "schema_version": 3,
        "detection_id": duplicate_detection["detection_id"],
        "kind": "word"
    });
    let duplicate_key = Uuid::now_v7();
    let (status, _, required) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(duplicate_key),
        duplicate_create.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{required}");
    assert_eq!(required["code"], "surface_match_acknowledgement_required");
    assert_eq!(required["meta"]["surface_match_page"]["schema_version"], 3);
    let create_token = required["meta"]["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .unwrap();

    let mut forged_create = duplicate_create.clone();
    forged_create["confirmed_surface_match_token"] = json!("forged-token");
    let (status, _, expired) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(duplicate_key),
        forged_create,
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{expired}");
    assert_eq!(expired["code"], "surface_match_snapshot_expired");

    let mut confirmed_create = duplicate_create;
    confirmed_create["confirmed_surface_match_token"] = json!(create_token);
    let (status, second_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(duplicate_key),
        Some(confirmed_create),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second_created}");

    let (status, policy_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "harbour"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{policy_detection}");
    let policy_token = policy_detection["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .unwrap();
    let policies = state.surface_policy_store_for_test();
    policies
        .transition(
            &pool,
            SurfacePolicyNameV2::SurfaceWarningAcknowledgement,
            false,
        )
        .await
        .unwrap();
    let (status, _, policy_changed) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        json!({
            "schema_version": 3,
            "detection_id": policy_detection["detection_id"],
            "kind": "word",
            "confirmed_surface_match_token": policy_token
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{policy_changed}");
    assert_eq!(policy_changed["code"], "surface_policy_changed");
    policies
        .transition(
            &pool,
            SurfacePolicyNameV2::SurfaceWarningAcknowledgement,
            true,
        )
        .await
        .unwrap();

    let (status, editing_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "dockyard"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{editing_detection}");
    let (status, editing_entry) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": editing_detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{editing_entry}");
    let editing_entry_id = editing_entry["word"]["id"].as_str().unwrap();
    let editing_forms = complete_v3_forms_fixture();
    let (status, impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{editing_entry_id}/steps/forms/impact"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "content": editing_forms
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    assert_eq!(impact["surface_match_page"]["schema_version"], 3);
    let forms_token = impact["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .unwrap();
    let forms_impact_token = impact["surface_match_page"]["impact_confirmation_token"]
        .as_str()
        .unwrap();

    let forms_candidate_variant_ids = impact["surface_match_page"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["match_kind"] == "form_variant_v3")
        .map(|item| Uuid::parse_str(item["match"]["variant_id"].as_str().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert!(!forms_candidate_variant_ids.is_empty());
    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET is_deleted = TRUE
        WHERE content_schema_version = 3
          AND source_node_id = ANY($1)
          AND is_deleted = FALSE
        "#,
    )
    .bind(&forms_candidate_variant_ids)
    .execute(&pool)
    .await
    .unwrap();
    let (status, _, disappeared) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{editing_entry_id}/steps/forms"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "complete",
            "confirmed_surface_match_token": forms_token,
            "confirmed_impact_token": forms_impact_token,
            "content": editing_forms
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{disappeared}");
    assert_eq!(disappeared["code"], "surface_matches_changed");
    assert!(disappeared["meta"]["surface_match_page"].is_null());
    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET is_deleted = FALSE
        WHERE content_schema_version = 3
          AND source_node_id = ANY($1)
        "#,
    )
    .bind(&forms_candidate_variant_ids)
    .execute(&pool)
    .await
    .unwrap();

    let (status, _, wrong_actor) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{editing_entry_id}/steps/forms"),
        &other_bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "complete",
            "confirmed_surface_match_token": forms_token,
            "confirmed_impact_token": forms_impact_token,
            "content": editing_forms
        }),
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{wrong_actor}");
    assert_eq!(wrong_actor["code"], "surface_match_snapshot_expired");

    let mut changed_forms = editing_forms;
    changed_forms["pos"][0]["form_groups"][0]["is_regular"] = json!(false);
    let (status, _, digest_changed) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{editing_entry_id}/steps/forms"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "complete",
            "confirmed_surface_match_token": forms_token,
            "confirmed_impact_token": forms_impact_token,
            "content": changed_forms
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{digest_changed}");
    assert_eq!(digest_changed["code"], "surface_matches_changed");
    let refreshed_token =
        digest_changed["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .unwrap();
    let refreshed_impact_token =
        digest_changed["meta"]["surface_match_page"]["impact_confirmation_token"]
            .as_str()
            .unwrap();
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{editing_entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "complete",
            "confirmed_surface_match_token": refreshed_token,
            "confirmed_impact_token": refreshed_impact_token,
            "content": changed_forms
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["word"]["revision"], 2);

    let evidence_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.entry_forms_surface_acknowledgements WHERE entry_id = $1",
    )
    .bind(Uuid::parse_str(editing_entry_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        evidence_count, 1,
        "successful V3 forms acknowledgement must be audited"
    );
}

#[sqlx::test]
async fn v3_publish_and_historical_v2_activation_require_bound_surface_tokens(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // The incumbent makes the V2 source entry acknowledge a real native-V3
    // surface before migration.
    create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let v2_draft = create_ready_draft(&state, &pool, &bearer, "harbour").await;
    let (status, v2_published) = publish_ready_confirming(&state, &bearer, &v2_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{v2_published}");
    let entry_id = Uuid::parse_str(v2_published["word"]["id"].as_str().unwrap()).unwrap();
    let source_v2_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    let batch_id = Uuid::now_v7();
    let plan = dry_run(&pool, batch_id, admin_id, Uuid::now_v7(), &[entry_id])
        .await
        .unwrap();
    assert_eq!(plan.eligible_entries, 1, "{plan:?}");
    approve(
        &pool,
        batch_id,
        admin_id,
        Uuid::now_v7(),
        &plan.manifest_digest,
    )
    .await
    .unwrap();
    apply(
        &pool,
        batch_id,
        admin_id,
        Uuid::now_v7(),
        &plan.manifest_digest,
    )
    .await
    .unwrap();
    let verified = verify(&pool, batch_id, admin_id, Uuid::now_v7())
        .await
        .unwrap();
    assert!(verified.ready, "{verified:?}");
    enable_publication_canary(&pool, batch_id, entry_id, admin_id, Uuid::now_v7())
        .await
        .unwrap();

    // A new peer after the old forms acknowledgement proves publish does not
    // reuse stale candidate membership.
    create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let (status, migrated) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{migrated}");
    assert_eq!(migrated["word"]["schema_version"], 3);
    let base_revision = migrated["word"]["revision"].as_i64().unwrap();
    let publish_body = json!({
        "schema_version": 3,
        "base_revision": base_revision
    });
    let (status, _, required) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        publish_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{required}");
    assert_eq!(required["code"], "surface_match_acknowledgement_required");
    assert_eq!(required["meta"]["surface_match_page"]["schema_version"], 3);
    let publish_token = required["meta"]["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .unwrap();

    let mut forged_publish = publish_body.clone();
    forged_publish["confirmed_surface_match_token"] = json!("forged-token");
    let (status, _, forged) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        forged_publish,
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{forged}");
    assert_eq!(forged["code"], "surface_match_snapshot_expired");

    let audits_before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit.admin_actions WHERE resource_id = $1 AND action = 'lexicon.surface_warning.acknowledge_command'",
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let candidate_variant_ids = required["meta"]["surface_match_page"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["match_kind"] == "form_variant_v3")
        .map(|item| Uuid::parse_str(item["match"]["variant_id"].as_str().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert!(!candidate_variant_ids.is_empty());
    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET is_deleted = TRUE
        WHERE content_schema_version = 3
          AND source_node_id = ANY($1)
          AND is_deleted = FALSE
        "#,
    )
    .bind(&candidate_variant_ids)
    .execute(&pool)
    .await
    .unwrap();

    let mut stale_after_removal = publish_body.clone();
    stale_after_removal["confirmed_surface_match_token"] = json!(publish_token);
    let (status, _, changed_without_replacement) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        stale_after_removal,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "{changed_without_replacement}"
    );
    assert_eq!(
        changed_without_replacement["code"],
        "surface_matches_changed"
    );
    assert!(
        changed_without_replacement["meta"]["surface_match_page"].is_null(),
        "candidate disappearance must reject the stale token without inventing an empty snapshot"
    );
    let current_after_rejected_token: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current_after_rejected_token, source_v2_publication_id);

    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET is_deleted = FALSE
        WHERE content_schema_version = 3
          AND source_node_id = ANY($1)
        "#,
    )
    .bind(&candidate_variant_ids)
    .execute(&pool)
    .await
    .unwrap();
    create_v3_with_complete_forms(&state, &pool, &bearer).await;

    let mut stale_after_addition = publish_body.clone();
    stale_after_addition["confirmed_surface_match_token"] = json!(publish_token);
    let (status, _, changed_with_replacement) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        stale_after_addition,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{changed_with_replacement}");
    assert_eq!(changed_with_replacement["code"], "surface_matches_changed");
    assert_eq!(
        changed_with_replacement["meta"]["surface_match_page"]["items"][0]["match_kind"],
        "form_variant_v3"
    );
    let refreshed_publish_token =
        changed_with_replacement["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .unwrap();
    let mut confirmed_publish = publish_body;
    confirmed_publish["confirmed_surface_match_token"] = json!(refreshed_publish_token);
    let (status, published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(confirmed_publish),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    let v3_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(v3_publication_id, source_v2_publication_id);

    let activate_body = json!({
        "schema_version": 3,
        "base_revision": published["word"]["revision"],
        "base_lifecycle_revision": published["word"]["lifecycle_revision"]
    });
    let activation_path =
        format!("{ROOT}/entries/{entry_id}/publications/{source_v2_publication_id}/activate");
    let (status, _, activation_required) = call_problem(
        &state,
        Method::POST,
        &activation_path,
        &bearer,
        Some(Uuid::now_v7()),
        activate_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{activation_required}");
    assert_eq!(
        activation_required["code"],
        "surface_match_acknowledgement_required"
    );
    assert_eq!(
        activation_required["meta"]["surface_match_page"]["schema_version"], 2,
        "V3 rollback to an immutable V2 snapshot must use truthful V2 match material"
    );
    let activation_token =
        activation_required["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .unwrap();
    let mut confirmed_activation = activate_body;
    confirmed_activation["confirmed_surface_match_token"] = json!(activation_token);
    let (status, activated) = call(
        &state,
        Method::POST,
        &activation_path,
        &bearer,
        Some(Uuid::now_v7()),
        Some(confirmed_activation),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{activated}");
    let current_publication_id: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current_publication_id, source_v2_publication_id);
    let current_versions: Vec<i16> = sqlx::query_scalar(
        r#"
        SELECT DISTINCT content_schema_version
        FROM lexicon.surface_sources
        WHERE entry_id = $1
          AND content_scope = 'current_publication'
          AND publication_id = $2
          AND is_deleted = FALSE
        "#,
    )
    .bind(entry_id)
    .bind(source_v2_publication_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(current_versions, vec![2]);

    let activate_v3_path =
        format!("{ROOT}/entries/{entry_id}/publications/{v3_publication_id}/activate");
    let activate_v3_body = json!({
        "schema_version": 3,
        "base_revision": activated["word"]["revision"],
        "base_lifecycle_revision": activated["word"]["lifecycle_revision"]
    });
    let (status, _, activate_v3_required) = call_problem(
        &state,
        Method::POST,
        &activate_v3_path,
        &bearer,
        Some(Uuid::now_v7()),
        activate_v3_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{activate_v3_required}");
    assert_eq!(
        activate_v3_required["code"],
        "surface_match_acknowledgement_required"
    );
    assert_eq!(
        activate_v3_required["meta"]["surface_match_page"]["schema_version"],
        3
    );
    assert_eq!(
        activate_v3_required["meta"]["surface_match_page"]["items"][0]["match_kind"],
        "form_variant_v3"
    );
    let mut confirmed_v3_activation = activate_v3_body;
    confirmed_v3_activation["confirmed_surface_match_token"] =
        activate_v3_required["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
    let (status, activated_v3) = call(
        &state,
        Method::POST,
        &activate_v3_path,
        &bearer,
        Some(Uuid::now_v7()),
        Some(confirmed_v3_activation),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{activated_v3}");
    let current_after_v3: Uuid =
        sqlx::query_scalar("SELECT current_publication_id FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current_after_v3, v3_publication_id);

    let audits_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit.admin_actions WHERE resource_id = $1 AND action = 'lexicon.surface_warning.acknowledge_command'",
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audits_after, audits_before + 3);
}

// Approved C1 HTTP cases: I12/I13/I14/I16 plus R01a's fail-closed gate.
// These tests intentionally prove that no V3 row is written before C2 storage exists.
#[sqlx::test]
async fn v3_create_unknown_version_and_publication_paths_fail_closed(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let (status, _, body) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": "colour"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "smart_lexicon_v3_detection_unavailable");

    let (status, content_type, body) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        json!({
            "schema_version": 3,
            "detection_id": Uuid::now_v7(),
            "kind": "word"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(body["code"], "smart_lexicon_v3_storage_unavailable");
    assert_eq!(body["status"], 503);
    let stored_entries: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored_entries, 0, "C1 capability gate 后不得产生 V3 词条");

    let (status, _, body) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        json!({
            "schema_version": 3,
            "detection_id": Uuid::now_v7(),
            "kind": "phrase"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "smart_lexicon_v3_storage_unavailable");

    let entry_id = Uuid::now_v7();
    let (status, _, body) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/validate"),
        &bearer,
        None,
        json!({"schema_version": "3", "base_revision": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_request_body");

    let (status, _, body) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/validate"),
        &bearer,
        None,
        json!({"schema_version": 4, "base_revision": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "unsupported_schema_version");

    let (status, _, body) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        json!({"schema_version": 3, "base_revision": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["code"],
        "smart_lexicon_v3_publication_requires_migration_canary"
    );

    let publication_id = Uuid::now_v7();
    let (status, _, body) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications/{publication_id}/activate"),
        &bearer,
        Some(Uuid::now_v7()),
        json!({
            "schema_version": 3,
            "base_revision": 1,
            "base_lifecycle_revision": 1
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["code"],
        "smart_lexicon_v3_publication_requires_migration_canary"
    );

    let (status, _, body) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        None,
        json!({"schema_version": 3, "base_revision": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["field"], "idempotency_key");

    let (status, _, body) = call_problem(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        Some(Uuid::now_v7()),
        json!({"schema_version": 3, "base_revision": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request_body");
    assert_eq!(body["field"], "base_revision");
}

#[sqlx::test]
async fn v3_forms_http_contract_reports_deep_membership_location_before_storage_gate(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let entry_id = Uuid::now_v7();
    let form_id = Uuid::now_v7();
    let group_id = Uuid::now_v7();
    let membership_id = Uuid::now_v7();
    let duplicate_membership_id = Uuid::now_v7();
    let mut body = json!({
        "schema_version": 3,
        "base_revision": 1,
        "intent": "complete",
        "content": {
            "pos": [{
                "pos_id": Uuid::now_v7(),
                "pos": "noun",
                "dialect_rules": {
                    "spelling_mode": "unified",
                    "phonetic_mode": "unified"
                },
                "forms": [{
                    "id": form_id,
                    "form_type": "base",
                    "regional_variants": {
                        "mode": "common",
                        "common": {
                            "id": Uuid::now_v7(),
                            "dialect": "common",
                            "spelling": "colour",
                            "origin": "manual",
                            "pronunciations": [{
                                "id": Uuid::now_v7(),
                                "dict_phonetic": "/kala/",
                                "actual_pron": "kala",
                                "style": "normal"
                            }]
                        }
                    }
                }],
                "form_groups": [{
                    "id": group_id,
                    "is_regular": true,
                    "members": [{"id": membership_id, "form_id": form_id}]
                }]
            }]
        }
    });
    let valid_body = body.clone();

    let (status, _, response) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{response}");
    assert_eq!(response["code"], "smart_lexicon_v3_storage_unavailable");

    body["content"]["pos"][0]["form_groups"][0]["members"] = json!([
        {"id": membership_id, "form_id": form_id},
        {"id": duplicate_membership_id, "form_id": form_id}
    ]);
    let (status, content_type, response) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        body,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(response["code"], "validation_failed");
    let issue = response["field_issues"]
        .as_array()
        .and_then(|issues| {
            issues
                .iter()
                .find(|issue| issue["code"] == "form_group_membership_invalid")
        })
        .expect("应返回重复 membership 的稳定 issue");
    assert_eq!(issue["schema_version"], 3);
    assert_eq!(issue["node_id"], duplicate_membership_id.to_string());
    assert_eq!(
        issue["node_location"]["membership_id"],
        duplicate_membership_id.to_string()
    );
    assert_eq!(issue["node_location"]["form_id"], form_id.to_string());
    assert_eq!(
        issue["node_location"]["form_group_id"],
        group_id.to_string()
    );

    // pronoun 这类 allowed_form_types=[] 的 POS 也能挂非 base 词形；原形留在组里，
    // 否则会先撞上「每组至少一个原形」这条规则，测不到 catalog 放行。
    let mut shared_form_type = valid_body.clone();
    shared_form_type["content"]["pos"][0]["pos"] = json!("pronoun");
    let comparative_form_id = Uuid::now_v7();
    let comparative_membership_id = Uuid::now_v7();
    let mut comparative_form = shared_form_type["content"]["pos"][0]["forms"][0].clone();
    comparative_form["id"] = json!(comparative_form_id);
    comparative_form["form_type"] = json!("comparative");
    comparative_form["regional_variants"]["common"]["id"] = json!(Uuid::now_v7());
    comparative_form["regional_variants"]["common"]["pronunciations"][0]["id"] =
        json!(Uuid::now_v7());
    shared_form_type["content"]["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .push(comparative_form);
    shared_form_type["content"]["pos"][0]["form_groups"][0]["members"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": comparative_membership_id,
            "form_id": comparative_form_id
        }));
    let (status, _, response) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        shared_form_type,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{response}");
    assert_eq!(response["code"], "smart_lexicon_v3_storage_unavailable");

    let mut invalid_dialect_rules = valid_body.clone();
    invalid_dialect_rules["content"]["pos"][0]["dialect_rules"] = json!({
        "spelling_mode": "distinguish",
        "phonetic_mode": "unified"
    });
    let (status, _, response) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        invalid_dialect_rules,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    let issue = response["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "dialect_rules_invalid")
        .expect("DU 组合应定位 POS dialect_rules");
    assert_eq!(issue["field"], "dialect_rules");
    assert_eq!(issue["node_id"], issue["node_location"]["pos_id"]);
    assert!(issue["node_location"].get("form_id").is_none());

    let mut missing_dialect_rules = valid_body.clone();
    missing_dialect_rules["content"]["pos"][0]
        .as_object_mut()
        .unwrap()
        .remove("dialect_rules");
    let (status, _, response) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        missing_dialect_rules,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    assert!(response["field_issues"].as_array().is_some_and(|issues| {
        issues.iter().any(|issue| {
            issue["code"] == "dialect_rules_invalid" && issue["field"] == "dialect_rules"
        })
    }));

    let mixed_form_id = Uuid::now_v7();
    let mut mixed_regional_modes = valid_body.clone();
    mixed_regional_modes["content"]["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": mixed_form_id,
            "form_type": "plural",
            "regional_variants": {
                "mode": "uk_us",
                "uk": {
                    "id": Uuid::now_v7(),
                    "dialect": "uk",
                    "spelling": "colours",
                    "origin": "manual",
                    "pronunciations": [{
                        "id": Uuid::now_v7(),
                        "dict_phonetic": "/kalaz/",
                        "actual_pron": "kalaz",
                        "style": "normal"
                    }]
                },
                "us": {
                    "id": Uuid::now_v7(),
                    "dialect": "us",
                    "spelling": "colors",
                    "origin": "manual",
                    "pronunciations": [{
                        "id": Uuid::now_v7(),
                        "dict_phonetic": "/kalarz/",
                        "actual_pron": "kalarz",
                        "style": "normal"
                    }]
                }
            }
        }));
    mixed_regional_modes["content"]["pos"][0]["form_groups"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": Uuid::now_v7(),
            "is_regular": false,
            "members": [{"id": Uuid::now_v7(), "form_id": mixed_form_id}]
        }));
    let (status, _, response) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        mixed_regional_modes,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    assert_eq!(response["code"], "validation_failed");
    let issue = response["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| {
            issue["code"] == "invalid_regional_variant_shape"
                && issue["node_id"] == mixed_form_id.to_string()
        })
        .expect("跨 form group 混用 common/uk_us 应定位冲突 form");
    assert_eq!(issue["field"], "regional_variants");
    assert_eq!(issue["node_location"]["form_id"], mixed_form_id.to_string());
    assert_eq!(issue["node_location"]["form_group_id"], Value::Null);
    assert!(issue["node_location"]["pos_id"].is_string());

    let mut unknown_form_type = valid_body.clone();
    unknown_form_type["content"]["pos"][0]["forms"][0]["form_type"] = json!("future_form_type");
    let (status, _, response) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        unknown_form_type,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    assert_eq!(response["code"], "validation_failed");
    let issue = response["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "invalid_form_type_for_part_of_speech")
        .expect("未知 fixed form type 应返回稳定 V3 issue");
    assert_eq!(issue["field"], "form_type");
    assert_eq!(issue["node_id"], form_id.to_string());
    assert_eq!(issue["node_location"]["form_id"], form_id.to_string());

    let mut missing_style = valid_body.clone();
    missing_style["content"]["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"]
        [0]
    .as_object_mut()
    .unwrap()
    .remove("style");
    let (status, _, response) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        missing_style,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    let issue = response["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["field"] == "style")
        .expect("complete 缺 style 应返回 pronunciation_required");
    assert_eq!(issue["code"], "pronunciation_required");
    assert!(issue["node_location"]["pronunciation_id"].is_string());

    let mut too_long = valid_body;
    too_long["content"]["pos"][0]["forms"][0]["regional_variants"]["common"]["spelling"] =
        json!("a".repeat(201));
    let (status, _, response) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        too_long,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    assert!(has_issue(&response, "content_limit_exceeded"));

    let stored_entries: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored_entries, 0);
}

#[sqlx::test]
async fn v3_meanings_extra_fields_and_node_limits_fail_before_storage_gate(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let entry_id = Uuid::now_v7();

    let (status, _, problem) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "save",
            "content": {"sense_groups": [], "pos": [], "unexpected": true}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(problem["code"], "invalid_request_body");

    let grammar_id = Uuid::now_v7();
    let variant_id = Uuid::now_v7();
    let meanings_with_rich_text = |text: String, liaisons: Vec<usize>| {
        json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "save",
            "content": {
                "sense_groups": [],
                "pos": [{
                    "pos_id": Uuid::now_v7(),
                    "grammar_structures": [{
                        "id": grammar_id,
                        "variants": [{
                            "id": variant_id,
                            "dialect": "common",
                            "content": {
                                "version": 1,
                                "text": text,
                                "spans": [],
                                "liaisons": liaisons
                            }
                        }]
                    }],
                    "senses": []
                }]
            }
        })
    };
    let (status, _, problem) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        meanings_with_rich_text("a".repeat(5000), vec![0; 2000]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "RichText 边界值应通过契约校验后命中 C1 storage gate：{problem}"
    );

    let (status, _, problem) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        meanings_with_rich_text("a".repeat(5001), Vec::new()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(has_issue(&problem, "content_limit_exceeded"));

    let (status, _, problem) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        meanings_with_rich_text(String::new(), vec![0; 2001]),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(has_issue(&problem, "content_limit_exceeded"));

    let sense_groups = (0..=2000)
        .map(|index| {
            json!({
                "id": Uuid::now_v7(),
                "name_zh": index.to_string(),
                "name_en": index.to_string()
            })
        })
        .collect::<Vec<_>>();
    let (status, _, problem) = call_problem(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        &bearer,
        None,
        json!({
            "schema_version": 3,
            "base_revision": 1,
            "intent": "save",
            "content": {"sense_groups": sense_groups, "pos": []}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(has_issue(&problem, "content_limit_exceeded"));
    assert_eq!(problem["field_issues"][0]["schema_version"], 3);
}

#[sqlx::test]
async fn publication_history_reads_immutable_v2_and_unknown_versions_fail_closed(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis);
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let published = create_and_publish(&state, &pool, &bearer, "historyword").await;
    let entry_id = published["word"]["id"].as_str().unwrap();
    let (publication_id, snapshot_hash): (Uuid, Vec<u8>) = sqlx::query_as(
        "SELECT id, snapshot_hash FROM lexicon.entry_publications WHERE entry_id = $1::uuid",
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let (status, history) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}/publications"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert_eq!(history["publications"].as_array().unwrap().len(), 1);
    assert_eq!(history["publications"][0]["schema_version"], 2);
    assert_eq!(history["publications"][0]["word"]["schema_version"], 2);
    assert_eq!(
        history["publications"][0]["publication_id"],
        publication_id.to_string()
    );
    assert_eq!(history["publications"][0]["is_current"], true);

    let (status, publication) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}/publications/{publication_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{publication}");
    assert_eq!(publication["publication"]["schema_version"], 2);
    assert_eq!(publication["publication"]["word"]["id"], entry_id);
    let hash_after_read: Vec<u8> =
        sqlx::query_scalar("SELECT snapshot_hash FROM lexicon.entry_publications WHERE id = $1")
            .bind(publication_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(snapshot_hash, hash_after_read, "历史读取不得改写 snapshot");

    sqlx::query(
        "ALTER TABLE lexicon.entry_publications DROP CONSTRAINT lexicon_entry_publications_schema_version_check",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE lexicon.entry_publications SET content_schema_version = 4 WHERE id = $1")
        .bind(publication_id)
        .execute(&pool)
        .await
        .unwrap();
    let (status, _, problem) = call_problem(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}/publications/{publication_id}"),
        &bearer,
        None,
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(problem["code"], "unsupported_schema_version");
}

async fn create_ready_v3_draft_with_sentences(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
    texts: &[&str],
) -> Value {
    let forms_saved = create_v3_with_complete_forms(state, pool, bearer).await;
    let entry_id = forms_saved["word"]["id"].as_str().unwrap();
    let mut meanings =
        complete_v3_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone());
    let sense_id = meanings["pos"][0]["senses"][0]["id"].clone();
    meanings["pos"][0]["senses"][0]["sentences"] = Value::Array(
        texts
            .iter()
            .map(|text| {
                json!({
                    "id": Uuid::now_v7(),
                    "level": "B1",
                    "en_text": {
                        "mode": "unified",
                        "common": {
                            "id": Uuid::now_v7(),
                            "origin": "manual",
                            "value": rich_text(text)
                        }
                    },
                    "zh_text_id": Uuid::now_v7(),
                    "zh_text": rich_text("测试译文。"),
                    "links": [{
                        "word_id": entry_id,
                        "sense_id": sense_id,
                        "role": "focus"
                    }]
                })
            })
            .collect(),
    );
    let (status, saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": forms_saved["word"]["revision"],
            "intent": "complete",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved
}

/// 把响应体里的词义搬成可写形状：例句关联是服务端投影，写请求不接受。
fn writable_v3_meanings(word: &Value) -> Value {
    strip_response_only_sentence_fields(word["word"]["meanings"].clone())
}

fn strip_response_only_sentence_fields(mut meanings: Value) -> Value {
    for sentence in meanings["pos"]
        .as_array_mut()
        .into_iter()
        .flatten()
        .flat_map(|pos| pos["senses"].as_array_mut().into_iter().flatten())
        .flat_map(|sense| sense["sentences"].as_array_mut().into_iter().flatten())
    {
        let sentence = sentence.as_object_mut().unwrap();
        sentence.remove("associations");
        sentence.remove("associations_state");
    }
    meanings
}

async fn save_v3_meanings_raw(
    state: &AppState,
    bearer: &str,
    entry_id: &str,
    base_revision: i64,
    content: Value,
) -> (StatusCode, Value) {
    call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": base_revision,
            "intent": "complete",
            "content": content
        })),
    )
    .await
}

async fn save_v3_meanings(state: &AppState, bearer: &str, word: &Value, meanings: Value) -> Value {
    let entry_id = word["word"]["id"].as_str().unwrap();
    let meanings = strip_response_only_sentence_fields(meanings);
    let (status, saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": word["word"]["revision"],
            "intent": "complete",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved
}

async fn publish_ready_v3(state: &AppState, bearer: &str, word: &Value) -> (StatusCode, Value) {
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
            "schema_version": 3,
            "base_revision": word["word"]["revision"]
        })),
    )
    .await
}

#[sqlx::test]
async fn sentence_target_discovery_returns_published_forms_with_base_and_sense_identity(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let location = create_and_publish(&state, &pool, &bearer, "location").await;
    let location_id = location["word"]["id"].clone();
    let location_sense_id = location["word"]["meanings"]["pos"][0]["senses"][0]["id"].clone();
    let location_base_id = location["word"]["forms"]["pos"][0]["base_form"]["id"].clone();

    let (status, discovered) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/sentence-targets/resolve"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "sentence_text": "The locations mark a central location.",
            "source_dialect": "common",
            "mode": "all_published_targets",
            "page_size_per_range": 20
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{discovered}");
    assert_eq!(discovered["schema_version"], 3);
    assert_eq!(discovered["completeness"], "complete");
    assert!(discovered["discovery_generation"].as_i64().unwrap() > 0);
    let ranges = discovered["range_results"].as_array().unwrap();
    assert_eq!(ranges.len(), 2, "复数与原形两个位置都应保留：{ranges:?}");
    for range in ranges {
        assert_eq!(range["published_total"], 1);
        let candidate = &range["published_matches"][0];
        assert_eq!(candidate["entry_id"], location_id);
        assert_eq!(candidate["base_form_id"], location_base_id);
        assert_eq!(candidate["senses"][0]["sense_id"], location_sense_id);
        assert_eq!(candidate["senses"][0]["base_form_id"], location_base_id);
        // V2 发布的目标做不了短语成分，词形一律不带可搭配的原形。
        assert!(
            candidate["forms"]
                .as_array()
                .unwrap()
                .iter()
                .all(|form| form["base_form_ids"] == json!([])),
            "V2 候选的 base_form_ids 应为空：{candidate}"
        );
        assert!(range.get("source_range").is_none());
    }
}

#[sqlx::test]
async fn discovery_capability_replaces_new_publish_time_implicit_associations(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    create_and_publish(&state, &pool, &bearer, "wall").await;
    let saved =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["A wall waits here."]).await;
    let (status, published) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_eq!(
        first_sentence(&published)["associations"],
        json!([]),
        "发现能力开启后发布不能再静默建立新关联"
    );
    assert_eq!(
        first_sentence(&published)["associations_state"],
        "resolved",
        "已扫描但尚未人工确认应是 resolved 空列表"
    );

    let (status, discovered) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/sentence-targets/resolve"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "sentence_text": "A wall waits here.",
            "source_dialect": "common",
            "mode": "all_published_targets"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{discovered}");
    assert!(
        discovered["range_results"]
            .as_array()
            .is_some_and(|ranges| ranges.iter().any(|range| {
                range["normalized_surface"] == "wall" && range["published_total"] == 1
            })),
        "一键发现仍必须返回 wall 候选：{discovered}"
    );
}

#[sqlx::test]
async fn v3_forms_resave_preserves_sentence_translation_node_roles(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let saved = create_ready_v3_draft_with_sentences(
        &state,
        &pool,
        &bearer,
        &["The same forms save must preserve this translation."],
    )
    .await;
    let entry_id = saved["word"]["id"].as_str().unwrap();
    let sentence_before = first_sentence(&saved).clone();

    let (status, repeated) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": saved["word"]["revision"],
            "intent": "complete",
            "content": saved["word"]["forms"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "词形重存不得破坏 V3 分层译文节点：{repeated}"
    );

    assert_eq!(
        first_sentence(&repeated)["id"],
        sentence_before["id"],
        "重复保存不得替换 sentence 节点身份"
    );
    assert_eq!(
        first_sentence(&repeated)["zh_text_id"],
        sentence_before["zh_text_id"],
        "重复保存不得替换中文译文别名节点身份"
    );
    assert_eq!(
        first_sentence(&repeated)["zh_translations"],
        sentence_before["zh_translations"]
    );
    let translation_id =
        Uuid::parse_str(first_sentence(&repeated)["zh_text_id"].as_str().unwrap()).unwrap();
    let stored_roles: (String, String) = sqlx::query_as(
        r#"
        SELECT node.node_role, translation.field_role
        FROM lexicon.nodes node
        JOIN lexicon.text_variants translation ON translation.id = node.id
        WHERE node.entry_id = $1::uuid AND node.id = $2::uuid
        "#,
    )
    .bind(entry_id)
    .bind(translation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        stored_roles,
        (
            "meanings.zh_translation_b1_b2:zh:common".to_owned(),
            "zh_translation_b1_b2".to_owned()
        )
    );
}

/// 发布路径的 V2 往返曾把每句多档 zh_translations 塌成 1 档（既有缺陷）。
/// 钉住：发布响应与不可变快照都保留全部三档；带 newly_bound 关联的发布同样保留。
#[sqlx::test]
async fn v3_publish_preserves_all_sentence_translation_bands(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let word = create_ready_v3_draft_with_sentences(
        &state,
        &pool,
        &bearer,
        &["A sentence with three bands."],
    )
    .await;
    let b_id = first_sentence(&word)["zh_translations"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // 存三档译文（乱序给，落库后按 c1_c2 → b1_b2 → a1_a2 排）。
    let mut meanings = word["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["sentences"][0]["zh_translations"] = json!([
        {"id": Uuid::now_v7(), "band": "a1_a2", "content": rich_text("高阶译文")},
        {"id": Uuid::now_v7(), "band": "c1_c2", "content": rich_text("初阶译文")},
        {"id": b_id, "band": "b1_b2", "content": rich_text("中阶译文")}
    ]);
    let saved = save_v3_meanings(&state, &bearer, &word, meanings).await;
    assert_eq!(
        first_sentence(&saved)["zh_translations"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "前置：保存后应有三档"
    );

    let (status, published) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");

    // 1) 发布响应保留三档，顺序与别名不变
    let bands: Vec<&str> = first_sentence(&published)["zh_translations"]
        .as_array()
        .unwrap_or_else(|| panic!("发布响应缺 zh_translations：{published}"))
        .iter()
        .map(|t| t["band"].as_str().unwrap())
        .collect();
    assert_eq!(
        bands,
        ["c1_c2", "b1_b2", "a1_a2"],
        "发布响应必须保留全部三档：{published}"
    );
    assert_eq!(first_sentence(&published)["zh_text_id"], b_id);

    // 2) 不可变发布快照也保留三档
    let publication_id = current_publication_id(
        &pool,
        Uuid::parse_str(published["word"]["id"].as_str().unwrap()).unwrap(),
    )
    .await;
    let snapshot_bands: Value = sqlx::query_scalar(
        "SELECT snapshot->'meanings'->'pos'->0->'senses'->0->'sentences'->0->'zh_translations' \
         FROM lexicon.entry_publications WHERE id = $1",
    )
    .bind(publication_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let snapshot_bands: Vec<&str> = snapshot_bands
        .as_array()
        .unwrap_or_else(|| panic!("快照缺 zh_translations：{snapshot_bands}"))
        .iter()
        .map(|t| t["band"].as_str().unwrap())
        .collect();
    assert_eq!(
        snapshot_bands,
        ["c1_c2", "b1_b2", "a1_a2"],
        "发布快照必须固化全部三档，否则下游只能读到 1 档"
    );

    // 3) newly_bound 分支（带待建关联词的发布）同样保留三档
    let source = create_ready_v3_draft_with_sentences(
        &state,
        &pool,
        &bearer,
        &["Another three-band sentence."],
    )
    .await;
    let src_sentence_b_id = first_sentence(&source)["zh_translations"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let pending_headword = format!("bandpending{}", admin_id.simple());
    let mut src_meanings = source["word"]["meanings"].clone();
    src_meanings["pos"][0]["senses"][0]["sentences"][0]["zh_translations"] = json!([
        {"id": Uuid::now_v7(), "band": "a1_a2", "content": rich_text("源高阶")},
        {"id": src_sentence_b_id, "band": "b1_b2", "content": rich_text("源中阶")},
        {"id": Uuid::now_v7(), "band": "c1_c2", "content": rich_text("源初阶")}
    ]);
    src_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "pending_target_headword": pending_headword,
        "score": "88.00"
    }]);
    let src_saved = save_v3_meanings(&state, &bearer, &source, src_meanings).await;
    let (status, src_published) = publish_ready_v3(&state, &bearer, &src_saved).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "带 newly_bound 的发布必须成功：{src_published}"
    );
    assert!(
        first_sentence(&src_published)["zh_translations"][0]["band"].is_string(),
        "前置：响应带 zh_translations"
    );
    let src_bands: Vec<&str> = first_sentence(&src_published)["zh_translations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["band"].as_str().unwrap())
        .collect();
    assert_eq!(
        src_bands,
        ["c1_c2", "b1_b2", "a1_a2"],
        "newly_bound 路径也必须保留三档（sync_canonical_meanings 前回填）：{src_published}"
    );
    assert!(
        src_published["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_word_id"]
            .is_string(),
        "前置：这条发布确实走了 newly_bound（待建关联词已被物化）：{src_published}"
    );
    // 投影也回填了三档
    let projected: Value = sqlx::query_scalar(
        "SELECT meanings->'pos'->0->'senses'->0->'sentences'->0->'zh_translations' \
         FROM lexicon.entry_editor_projection WHERE entry_id = $1",
    )
    .bind(Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        projected.as_array().map(Vec::len),
        Some(3),
        "newly_bound 的 sync_canonical_meanings 覆盖投影时必须回填三档"
    );
}

#[sqlx::test]
async fn v3_sentence_translations_save_three_bands_and_round_trip(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let word = create_ready_v3_draft_with_sentences(
        &state,
        &pool,
        &bearer,
        &["A sentence with three translations."],
    )
    .await;
    let entry_id = word["word"]["id"].as_str().unwrap();
    let sentence_id = first_sentence(&word)["id"].as_str().unwrap();
    let initial = &first_sentence(&word)["zh_translations"];
    assert_eq!(initial.as_array().unwrap().len(), 1);
    assert_eq!(initial[0]["band"], "b1_b2");

    let c_id = Uuid::now_v7();
    let b_id = initial[0]["id"].as_str().unwrap().to_owned();
    let a_id = Uuid::now_v7();
    let mut meanings = word["word"]["meanings"].clone();
    let sentence = &mut meanings["pos"][0]["senses"][0]["sentences"][0];
    sentence["zh_translations"] = json!([
        {"id": a_id, "band": "a1_a2", "content": rich_text("高阶译文")},
        {"id": c_id, "band": "c1_c2", "content": rich_text("初阶译文")},
        {"id": b_id, "band": "b1_b2", "content": rich_text("中阶译文")}
    ]);
    let saved = save_v3_meanings(&state, &bearer, &word, meanings).await;
    let translations = first_sentence(&saved)["zh_translations"]
        .as_array()
        .unwrap();
    assert_eq!(
        translations
            .iter()
            .map(|translation| translation["band"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["c1_c2", "b1_b2", "a1_a2"]
    );
    assert_eq!(first_sentence(&saved)["zh_text_id"], b_id);
    assert_eq!(first_sentence(&saved)["zh_text"]["text"], "中阶译文");

    let stored: Vec<(Uuid, String, String)> = sqlx::query_as(
        r#"
        SELECT id, field_role, plain_text
        FROM lexicon.text_variants
        WHERE entry_id = $1::uuid
          AND owner_node_id = $2::uuid
          AND field_role LIKE 'zh_translation_%'
        ORDER BY sort_order
        "#,
    )
    .bind(entry_id)
    .bind(sentence_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(stored.len(), 3);
    assert_eq!(
        stored
            .iter()
            .map(|(_, role, text)| (role.as_str(), text.as_str()))
            .collect::<Vec<_>>(),
        [
            ("zh_translation_c1_c2", "初阶译文"),
            ("zh_translation_b1_b2", "中阶译文"),
            ("zh_translation_a1_a2", "高阶译文"),
        ]
    );
    let duplicate_band = sqlx::query(
        r#"
        UPDATE lexicon.text_variants
        SET field_role = 'zh_translation_b1_b2'
        WHERE id = $1
        "#,
    )
    .bind(c_id)
    .execute(&pool)
    .await;
    assert!(
        duplicate_band.is_err(),
        "数据库 slot 唯一约束必须拒绝同一句重复 translation band"
    );

    let (status, reloaded) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reloaded}");
    assert_eq!(
        first_sentence(&reloaded)["zh_translations"],
        first_sentence(&saved)["zh_translations"]
    );

    let mut changed_level_meanings = reloaded["word"]["meanings"].clone();
    changed_level_meanings["pos"][0]["senses"][0]["sentences"][0]["level"] = json!("A1");
    let changed_level = save_v3_meanings(&state, &bearer, &reloaded, changed_level_meanings).await;
    assert_eq!(
        first_sentence(&changed_level)["zh_text_id"],
        a_id.to_string()
    );
    assert_eq!(
        first_sentence(&changed_level)["zh_translations"],
        first_sentence(&saved)["zh_translations"]
    );

    let mut compatibility_meanings = changed_level["word"]["meanings"].clone();
    compatibility_meanings["pos"][0]["senses"][0]["sentences"][0]
        .as_object_mut()
        .unwrap()
        .remove("zh_translations");
    let compatibility_saved =
        save_v3_meanings(&state, &bearer, &changed_level, compatibility_meanings).await;
    assert_eq!(
        first_sentence(&compatibility_saved)["zh_translations"],
        first_sentence(&changed_level)["zh_translations"],
        "旧客户端缺少 zh_translations 时必须保留已有三档译文"
    );

    let mut explicit_clear = compatibility_saved["word"]["meanings"].clone();
    explicit_clear["pos"][0]["senses"][0]["sentences"][0]["zh_translations"] = json!([]);
    let explicitly_cleared =
        save_v3_meanings(&state, &bearer, &compatibility_saved, explicit_clear).await;
    let cleared_translations = first_sentence(&explicitly_cleared)["zh_translations"]
        .as_array()
        .unwrap();
    assert_eq!(cleared_translations.len(), 1);
    assert_eq!(cleared_translations[0]["id"], a_id.to_string());
    assert_eq!(cleared_translations[0]["content"]["text"], "高阶译文");
}

#[sqlx::test]
async fn v3_create_persists_explicit_final_headwords_and_keeps_legacy_body_compatible(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let detected_surface = format!("v3final{}", admin_id.simple());
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": detected_surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    assert_eq!(detection["builtin_dictionary"]["status"], "not_found");

    let explicit_final = format!("edited{}", admin_id.simple());
    let explicit_key = Uuid::now_v7();
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(explicit_key),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word",
            "headwords": {"mode": "unified", "common": explicit_final}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        created["word"]["presentation"]["matched_surfaces"],
        json!([explicit_final])
    );
    let explicit_entry = Uuid::parse_str(created["word"]["id"].as_str().unwrap()).unwrap();
    let persisted: (Value, Vec<String>) = sqlx::query_as(
        "SELECT initial_headwords, initial_headword_keys FROM lexicon.v3_entry_state WHERE entry_id = $1",
    )
    .bind(explicit_entry)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        persisted.0,
        json!({"mode": "unified", "common": explicit_final})
    );
    assert_eq!(
        persisted.1,
        vec![
            format!("uk:{explicit_final}"),
            format!("us:{explicit_final}")
        ]
    );
    let (status, changed_replay) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(explicit_key),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word",
            "headwords": {"mode": "unified", "common": format!("changed{admin_id}")}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{changed_replay}");
    assert_eq!(changed_replay["code"], "idempotency_conflict");

    let duplicate_detection_surface = format!("v3duplicate{}", admin_id.simple());
    let (status, duplicate_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": duplicate_detection_surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{duplicate_detection}");
    let (status, duplicate) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": duplicate_detection["detection_id"],
            "kind": "word",
            "headwords": {"mode": "unified", "common": explicit_final}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");
    assert_eq!(duplicate["code"], "duplicate_word");

    let legacy_surface = format!("v3legacy{}", admin_id.simple());
    let (status, legacy_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": legacy_surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{legacy_detection}");
    let (status, legacy_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": legacy_detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{legacy_created}");
    let legacy_entry = Uuid::parse_str(legacy_created["word"]["id"].as_str().unwrap()).unwrap();
    let legacy_persisted: (Value, Vec<String>) = sqlx::query_as(
        "SELECT initial_headwords, initial_headword_keys FROM lexicon.v3_entry_state WHERE entry_id = $1",
    )
    .bind(legacy_entry)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        legacy_persisted.0,
        json!({"mode": "unified", "common": legacy_surface})
    );
    assert_eq!(
        legacy_persisted.1,
        vec![
            format!("uk:{legacy_surface}"),
            format!("us:{legacy_surface}")
        ]
    );

    let (status, sequential_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": legacy_surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sequential_detection}");
    let (status, sequential_duplicate) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": sequential_detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{sequential_duplicate}");
    assert_eq!(sequential_duplicate["code"], "duplicate_word");

    sqlx::query(
        "UPDATE lexicon.v3_entry_state SET initial_headwords = NULL, initial_headword_keys = NULL WHERE entry_id = $1",
    )
    .bind(legacy_entry)
    .execute(&pool)
    .await
    .unwrap();

    let (status, null_fallback_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": legacy_surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{null_fallback_detection}");
    let (status, null_fallback_duplicate) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": null_fallback_detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{null_fallback_duplicate}");
    assert_eq!(null_fallback_duplicate["code"], "duplicate_word");

    let second_legacy_surface = format!("v3legacysecond{}", admin_id.simple());
    let (status, second_legacy_detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": second_legacy_surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second_legacy_detection}");
    let (status, second_legacy_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": second_legacy_detection["detection_id"],
            "kind": "word"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{second_legacy_created}");
    let second_legacy_entry =
        Uuid::parse_str(second_legacy_created["word"]["id"].as_str().unwrap()).unwrap();
    let second_legacy_persisted: (Value, Vec<String>) = sqlx::query_as(
        "SELECT initial_headwords, initial_headword_keys FROM lexicon.v3_entry_state WHERE entry_id = $1",
    )
    .bind(second_legacy_entry)
    .fetch_one(&pool)
    .await
    .unwrap();

    let dry_run = tsz_rust::lexicon::v3_initial_headword_backfill::dry_run(&pool)
        .await
        .unwrap();
    assert_eq!(dry_run.scanned, 1);
    assert_eq!(dry_run.ready, 1);
    assert_eq!(dry_run.applied, 0);
    assert!(dry_run.blockers.is_empty());

    let mut legacy_writer = pool.begin().await.unwrap();
    LexiconRepository::lock_surface_policy_writer(&mut legacy_writer)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE lexicon.v3_entry_state SET initial_headwords = NULL, initial_headword_keys = NULL WHERE entry_id = $1",
    )
    .bind(second_legacy_entry)
    .execute(&mut *legacy_writer)
    .await
    .unwrap();
    let apply_pool = pool.clone();
    let stale_digest = dry_run.manifest_digest.clone();
    let apply_task = tokio::spawn(async move {
        tsz_rust::lexicon::v3_initial_headword_backfill::apply(&apply_pool, &stale_digest).await
    });
    await_database_lock_waiters(&pool, 1).await;
    legacy_writer.commit().await.unwrap();
    let stale_manifest = tokio::time::timeout(CONCURRENCY_TIMEOUT, apply_task)
        .await
        .expect("backfill should resume after the old writer commits")
        .unwrap()
        .unwrap_err();
    assert!(
        stale_manifest
            .to_string()
            .contains("v3_initial_headword_backfill_manifest_mismatch")
    );
    let refreshed_dry_run = tsz_rust::lexicon::v3_initial_headword_backfill::dry_run(&pool)
        .await
        .unwrap();
    assert_eq!(refreshed_dry_run.scanned, 2);
    let applied = tsz_rust::lexicon::v3_initial_headword_backfill::apply(
        &pool,
        &refreshed_dry_run.manifest_digest,
    )
    .await
    .unwrap();
    assert_eq!(applied.applied, 2);
    assert_eq!(applied.manifest_digest, refreshed_dry_run.manifest_digest);
    let backfilled: (Value, Vec<String>) = sqlx::query_as(
        "SELECT initial_headwords, initial_headword_keys FROM lexicon.v3_entry_state WHERE entry_id = $1",
    )
    .bind(legacy_entry)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(backfilled, legacy_persisted);
    let second_backfilled: (Value, Vec<String>) = sqlx::query_as(
        "SELECT initial_headwords, initial_headword_keys FROM lexicon.v3_entry_state WHERE entry_id = $1",
    )
    .bind(second_legacy_entry)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(second_backfilled, second_legacy_persisted);
}

#[sqlx::test]
async fn v3_create_and_read_expose_the_original_detection_basis_dialect(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let us = format!("center{}", admin_id.simple());
    let uk = format!("centre{}", admin_id.simple());

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": us
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");

    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word",
            "headwords": {
                "mode": "distinguish",
                "uk": uk,
                "us": us,
                "source_dialect": "uk"
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["word"]["detection_basis_dialect"], "us");

    let entry_id = created["word"]["id"].as_str().unwrap();
    let (status, read_back) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read_back}");
    assert_eq!(read_back["word"]["detection_basis_dialect"], "us");
}

#[sqlx::test]
async fn concurrent_legacy_v3_empty_skeleton_creation_allows_only_one_entry(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let surface = format!("v3legacyrace{}", admin_id.simple());
    let mut detections = Vec::new();
    for _ in 0..2 {
        let (status, detection) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/detections"),
            &bearer,
            None,
            Some(json!({
                "schema_version": 3,
                "language": "en",
                "kind": "word",
                "surface": surface
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{detection}");
        detections.push(detection);
    }
    let create_path = format!("{ROOT}/entries");
    let first = call(
        &state,
        Method::POST,
        &create_path,
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detections[0]["detection_id"],
            "kind": "word"
        })),
    );
    let second = call(
        &state,
        Method::POST,
        &create_path,
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detections[1]["detection_id"],
            "kind": "word"
        })),
    );
    let (first, second) = tokio::join!(first, second);
    let (created, duplicate) = match (first, second) {
        ((StatusCode::CREATED, created), (StatusCode::CONFLICT, duplicate))
        | ((StatusCode::CONFLICT, duplicate), (StatusCode::CREATED, created)) => {
            (created, duplicate)
        }
        (first, second) => {
            panic!("concurrent legacy creates should produce one entry: {first:?} {second:?}")
        }
    };
    assert!(created["word"]["id"].is_string());
    assert_eq!(duplicate["code"], "duplicate_word");
    let count = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM lexicon.v3_entry_state state
        JOIN lexicon.entries entry ON entry.id = state.entry_id
        WHERE entry.detection_snapshot ->> 'normalized_surface' = $1
        "#,
    )
    .bind(surface)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn clearing_v3_forms_cannot_create_duplicate_active_hidden_initial_headwords(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let sequential_surface = format!("v3clearhidden{}", admin_id.simple());
    let first = create_legacy_v3_empty_skeleton(&state, &bearer, &sequential_surface).await;
    let forms = complete_v3_forms_fixture();
    let (_impact, saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        &first.to_string(),
        1,
        "save",
        forms.clone(),
    )
    .await;
    assert_eq!(saved["word"]["revision"], 2);
    let _second = create_legacy_v3_empty_skeleton(&state, &bearer, &sequential_surface).await;
    let (status, rejected_clear) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{first}/steps/forms"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 2,
            "intent": "save",
            "content": {"pos": []}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected_clear}");
    assert_eq!(rejected_clear["code"], "duplicate_word");

    let concurrent_surface = format!("v3clearhiddenrace{}", admin_id.simple());
    let editing = create_legacy_v3_empty_skeleton(&state, &bearer, &concurrent_surface).await;
    let (_impact, saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        &editing.to_string(),
        1,
        "save",
        complete_v3_forms_fixture(),
    )
    .await;
    assert_eq!(saved["word"]["revision"], 2);
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": concurrent_surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let clear_path = format!("{ROOT}/entries/{editing}/steps/forms");
    let create_path = format!("{ROOT}/entries");
    let clear = call(
        &state,
        Method::PUT,
        &clear_path,
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 2,
            "intent": "save",
            "content": {"pos": []}
        })),
    );
    let create = call(
        &state,
        Method::POST,
        &create_path,
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word"
        })),
    );
    let (clear, create) = tokio::join!(clear, create);
    match (clear, create) {
        ((StatusCode::OK, _), (StatusCode::CONFLICT, duplicate))
        | ((StatusCode::CONFLICT, duplicate), (StatusCode::CREATED, _)) => {
            assert!(
                matches!(
                    duplicate["code"].as_str(),
                    Some("duplicate_word" | "downstream_confirmation_required")
                ),
                "并发 loser 必须被重复或下游影响确认安全阻断：{duplicate}"
            );
        }
        (clear, create) => {
            panic!("clear/create should serialize to one hidden owner: {clear:?} {create:?}")
        }
    }
    let hidden_owners: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM lexicon.v3_entry_state state
        JOIN lexicon.entries entry ON entry.id = state.entry_id
        WHERE entry.kind = 'word'
          AND entry.archived_at IS NULL
          AND state.initial_headword_keys && $1
          AND NOT EXISTS (
              SELECT 1
              FROM lexicon.surface_sources source
              WHERE source.entry_id = state.entry_id
                AND source.is_deleted = FALSE
          )
        "#,
    )
    .bind(vec![
        format!("uk:{concurrent_surface}"),
        format!("us:{concurrent_surface}"),
    ])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(hidden_owners, 1);
}

#[sqlx::test]
async fn initial_headword_backfill_blocks_historical_duplicate_empty_skeletons(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let first = create_legacy_v3_empty_skeleton(
        &state,
        &bearer,
        &format!("v3backfillduplicatea{}", admin_id.simple()),
    )
    .await;
    let second = create_legacy_v3_empty_skeleton(
        &state,
        &bearer,
        &format!("v3backfillduplicateb{}", admin_id.simple()),
    )
    .await;
    sqlx::query(
        r#"
        UPDATE lexicon.entries target
        SET detection_snapshot = source.detection_snapshot
        FROM lexicon.entries source
        WHERE target.id = $1
          AND source.id = $2
        "#,
    )
    .bind(second)
    .bind(first)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        UPDATE lexicon.v3_entry_state
        SET initial_headwords = NULL,
            initial_headword_keys = NULL
        WHERE entry_id = ANY($1)
        "#,
    )
    .bind(vec![first, second])
    .execute(&pool)
    .await
    .unwrap();

    let dry_run = tsz_rust::lexicon::v3_initial_headword_backfill::dry_run(&pool)
        .await
        .unwrap();
    assert_eq!(dry_run.scanned, 2);
    assert_eq!(dry_run.ready, 0);
    assert_eq!(dry_run.blockers.len(), 2);
    assert!(
        dry_run
            .blockers
            .iter()
            .all(|blocker| blocker.reason == "duplicate_active_empty_skeleton")
    );
    let error =
        tsz_rust::lexicon::v3_initial_headword_backfill::apply(&pool, &dry_run.manifest_digest)
            .await
            .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("v3_initial_headword_backfill_blocked")
    );
    let remaining: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM lexicon.v3_entry_state
        WHERE entry_id = ANY($1)
          AND initial_headword_keys IS NULL
        "#,
    )
    .bind(vec![first, second])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, 2);

    let duplicate_surface: String = sqlx::query_scalar(
        "SELECT detection_snapshot ->> 'normalized_surface' FROM lexicon.entries WHERE id = $1",
    )
    .bind(first)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        UPDATE lexicon.v3_entry_state
        SET initial_headwords = $2,
            initial_headword_keys = $3
        WHERE entry_id = ANY($1)
        "#,
    )
    .bind(vec![first, second])
    .bind(json!({"mode": "unified", "common": duplicate_surface}))
    .bind(vec![
        format!("uk:{duplicate_surface}"),
        format!("us:{duplicate_surface}"),
    ])
    .execute(&pool)
    .await
    .unwrap();
    let non_null_dry_run = tsz_rust::lexicon::v3_initial_headword_backfill::dry_run(&pool)
        .await
        .unwrap();
    assert_eq!(non_null_dry_run.ready, 0);
    assert_eq!(non_null_dry_run.blockers.len(), 2);
    let error = tsz_rust::lexicon::v3_initial_headword_backfill::apply(
        &pool,
        &non_null_dry_run.manifest_digest,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("v3_initial_headword_backfill_blocked")
    );
}

#[sqlx::test]
async fn v3_create_rebinds_dictionary_base_forms_to_explicit_regional_headwords(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let surface = format!("v3regional{}", admin_id.simple());
    seed_dictionary_word(&pool, &surface).await;

    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    assert_eq!(detection["builtin_dictionary"]["status"], "matched");

    let uk = format!("uk{}", admin_id.simple());
    let us = format!("us{}", admin_id.simple());
    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word",
            "headwords": {
                "mode": "distinguish",
                "uk": uk.clone(),
                "us": us.clone(),
                "source_dialect": "uk"
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        created["word"]["presentation"]["matched_surfaces"],
        json!([uk, us])
    );
    let base = created["word"]["forms"]["pos"][0]["forms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|form| form["form_type"] == "base")
        .expect("dictionary create should contain a base form");
    assert_eq!(base["regional_variants"]["mode"], "uk_us");
    assert_eq!(base["regional_variants"]["uk"]["spelling"], uk);
    assert_eq!(base["regional_variants"]["us"]["spelling"], us);
    assert_eq!(
        created["word"]["forms"]["pos"][0]["dialect_rules"],
        json!({"spelling_mode": "distinguish", "phonetic_mode": "distinguish"})
    );
}

#[sqlx::test]
async fn v3_create_rejects_invalid_explicit_headwords_without_consuming_detection(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let surface = format!("v3invalid{}", admin_id.simple());
    let (status, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let key = Uuid::now_v7();
    let (status, invalid) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(key),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word",
            "headwords": {"mode": "unified", "common": "苹果"}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
    assert_eq!(invalid["code"], "invalid_headword");

    let (status, created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(key),
        Some(json!({
            "schema_version": 3,
            "detection_id": detection["detection_id"],
            "kind": "word",
            "headwords": {"mode": "unified", "common": surface}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
}

async fn search_component_targets(
    state: &AppState,
    bearer: &str,
    body: Value,
) -> (StatusCode, Value) {
    call(
        state,
        Method::POST,
        &format!("{ROOT}/entries/component-targets/search"),
        bearer,
        None,
        Some(body),
    )
    .await
}

fn component_match_entry_ids(response: &Value) -> HashSet<String> {
    response["matches"]
        .as_array()
        .expect("matches 必须是数组")
        .iter()
        .map(|candidate| candidate["entry_id"].as_str().unwrap().to_owned())
        .collect()
}

#[sqlx::test]
async fn component_target_search_matches_published_surfaces_and_hides_drafts_and_archived(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 已发布单词 harbour/harbor，另挂一条复数 harbours/harbors：屈折词形也进 surface_sources，
    // 搜 "harbours" 应当命中同一个原形词条。
    let word_created = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let word_entry_id = word_created["word"]["id"].as_str().unwrap().to_owned();
    let mut forms = word_created["word"]["forms"].clone();
    let base_form_id = forms["pos"][0]["forms"][0]["id"].clone();
    let plural_id = Uuid::now_v7();
    forms["pos"][0]["forms"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": plural_id,
            "form_type": "plural",
            "regional_variants": {
                "mode": "uk_us",
                "uk": {
                    "id": Uuid::now_v7(),
                    "dialect": "uk",
                    "spelling": "harbours",
                    "origin": "manual",
                    "pronunciations": [{
                        "id": Uuid::now_v7(),
                        "dict_phonetic": "/ˈhɑːbəz/",
                        "actual_pron": "hɑːbəz",
                        "style": "normal"
                    }]
                },
                "us": {
                    "id": Uuid::now_v7(),
                    "dialect": "us",
                    "spelling": "harbors",
                    "origin": "manual",
                    "pronunciations": [{
                        "id": Uuid::now_v7(),
                        "dict_phonetic": "/ˈhɑrbərz/",
                        "actual_pron": "hɑrbərz",
                        "style": "normal"
                    }]
                }
            }
        }));
    forms["pos"][0]["form_groups"][0]["members"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": Uuid::now_v7(), "form_id": plural_id}));
    let (_, forms_saved) = save_v3_forms_after_impact(
        &state,
        &bearer,
        &word_entry_id,
        word_created["word"]["revision"].as_i64().unwrap(),
        "complete",
        forms,
    )
    .await;
    let meanings_saved = save_v3_meanings(
        &state,
        &bearer,
        &forms_saved,
        complete_v3_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone()),
    )
    .await;
    let (status, word_published) = publish_ready_v3(&state, &bearer, &meanings_saved).await;
    assert_eq!(status, StatusCode::CREATED, "{word_published}");

    let (phrase_published, _) =
        create_published_v3_phrase(&state, &pool, &bearer, "harbour club", json!([])).await;
    let phrase_entry_id = phrase_published["word"]["id"].as_str().unwrap().to_owned();

    // 草稿：只保存词形步，不发布。成分关联要存 target_publication_id，草稿没有发布快照。
    let (draft_entry_id, draft_forms) =
        create_v3_phrase_draft(&state, &bearer, "harbour sketch").await;
    save_v3_forms_after_impact(&state, &bearer, &draft_entry_id, 1, "complete", draft_forms).await;

    // 已发布后归档：surface 行还在，但 entry.archived_at 已非空。
    let (archived_published, _) =
        create_published_v3_phrase(&state, &pool, &bearer, "harbour attic", json!([])).await;
    let archived_entry_id = archived_published["word"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{archived_entry_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": archived_published["word"]["revision"],
            "base_lifecycle_revision": archived_published["word"]["lifecycle_revision"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "归档失败：{archived}");

    let (status, found) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbour"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["schema_version"], 3);
    assert_eq!(found["truncated"], false);
    let entry_ids = component_match_entry_ids(&found);
    assert_eq!(
        found["total"].as_u64().unwrap(),
        found["matches"].as_array().unwrap().len() as u64,
        "未截断时 total 就是返回条数：{found}"
    );
    assert!(
        entry_ids.contains(&word_entry_id) && entry_ids.contains(&phrase_entry_id),
        "已发布的单词与短语都该命中：{found}"
    );
    assert!(
        !entry_ids.contains(&draft_entry_id),
        "草稿不得进成分目标候选：{found}"
    );
    assert!(
        !entry_ids.contains(&archived_entry_id),
        "归档词条不得进成分目标候选：{found}"
    );

    let word_candidate = found["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["entry_id"] == json!(word_entry_id))
        .expect("单词候选应在结果里");
    assert_eq!(word_candidate["kind"], "word");
    assert_eq!(
        word_candidate["matches"],
        json!([]),
        "关键字检索没有句子区间，候选不得带命中证据：{word_candidate}"
    );
    assert!(
        !word_candidate["senses"].as_array().unwrap().is_empty(),
        "候选必须带词义供级联第三层：{word_candidate}"
    );
    let candidate_forms = word_candidate["forms"].as_array().unwrap();
    assert!(
        candidate_forms
            .iter()
            .any(|form| form["form_id"] == json!(plural_id)),
        "词形清单应覆盖该词性下全部词形：{word_candidate}"
    );
    assert!(
        candidate_forms.iter().all(|form| form["base_form_ids"]
            .as_array()
            .is_some_and(|ids| !ids.is_empty())),
        "V3 目标的每个词形都必须自带 base_form_ids：{word_candidate}"
    );
    assert!(
        candidate_forms.iter().any(|form| form["base_form_ids"]
            .as_array()
            .unwrap()
            .contains(&base_form_id)),
        "复数应指回同组原形：{word_candidate}"
    );

    // 屈折词形：搜 "harbours" 命中的仍是原形词条本身。
    let (status, inflected) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbours"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{inflected}");
    assert!(
        component_match_entry_ids(&inflected).contains(&word_entry_id),
        "屈折词形应命中原形词条：{inflected}"
    );

    for (kind, expected, unexpected) in [
        ("word", &word_entry_id, &phrase_entry_id),
        ("phrase", &phrase_entry_id, &word_entry_id),
    ] {
        let (status, filtered) = search_component_targets(
            &state,
            &bearer,
            json!({"schema_version": 3, "q": "harbour", "kind": kind}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{filtered}");
        let ids = component_match_entry_ids(&filtered);
        assert!(ids.contains(expected), "kind={kind} 应保留：{filtered}");
        assert!(
            !ids.contains(unexpected),
            "kind={kind} 应过滤掉：{filtered}"
        );
    }

    // 通配符字面量：% 被转义，只会命中真的带百分号的词面，也就是没有。
    let (status, wildcard) =
        search_component_targets(&state, &bearer, json!({"schema_version": 3, "q": "%"})).await;
    assert_eq!(status, StatusCode::OK, "{wildcard}");
    assert_eq!(
        wildcard["matches"],
        json!([]),
        "% 不得当通配符用：{wildcard}"
    );
    assert_eq!(wildcard["total"], 0);
    assert_eq!(wildcard["truncated"], false);

    let (status, paged) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbour", "page_size": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{paged}");
    assert_eq!(paged["matches"].as_array().unwrap().len(), 1);
    assert_eq!(paged["truncated"], true, "超出 page_size 必须标 truncated");
    assert!(paged["total"].as_u64().unwrap() > 1, "{paged}");

    for body in [
        json!({"schema_version": 3, "q": " harbour"}),
        json!({"schema_version": 3, "q": "harbour "}),
        json!({"schema_version": 3, "q": ""}),
        json!({"schema_version": 3, "q": "h".repeat(101)}),
    ] {
        let (status, rejected) = search_component_targets(&state, &bearer, body.clone()).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body} 应被拒：{rejected}"
        );
        assert_eq!(rejected["code"], "validation_failed", "{rejected}");
        assert_eq!(rejected["meta"]["code"], "q", "{rejected}");
    }
    for body in [
        json!({"schema_version": 3, "q": "harbour", "page_size": 0}),
        json!({"schema_version": 3, "q": "harbour", "page_size": 201}),
    ] {
        let (status, rejected) = search_component_targets(&state, &bearer, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} 应被拒：{rejected}");
        assert_eq!(rejected["code"], "invalid_query", "{rejected}");
        assert_eq!(rejected["field"], "page_size", "{rejected}");
    }
}

#[sqlx::test]
async fn component_target_search_shares_the_discovery_capability_gate_with_resolve(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags {
            sentence_target_discovery: false,
            ..SmartLexiconV3Flags::all_enabled()
        });
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let (status, gated) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbour"}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{gated}");
    assert_eq!(gated["code"], "smart_lexicon_v3_storage_unavailable");

    let (status, resolve_gated) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/sentence-targets/resolve"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "sentence_text": "harbour",
            "source_dialect": "common",
            "mode": "all_published_targets"
        })),
    )
    .await;
    assert_eq!(
        (status, &resolve_gated["code"]),
        (StatusCode::SERVICE_UNAVAILABLE, &gated["code"]),
        "能力门关闭时两条端点必须给同一种拒绝：{resolve_gated}"
    );
}

#[sqlx::test]
async fn component_target_search_flags_truncated_when_the_scan_row_cap_is_hit(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let (published, _) =
        create_published_v3_phrase(&state, &pool, &bearer, "harbour club", json!([])).await;
    let entry_id = Uuid::parse_str(published["word"]["id"].as_str().unwrap()).unwrap();

    // truncated 有两条独立成因，这里钉的是「一次取回的词面行触顶」那条：把同一条已发布词面
    // 复制到 2000 行上限，候选去重后仍然只有几条，但结果必须标 truncated。
    let cloned = sqlx::query(
        r#"
        INSERT INTO lexicon.surface_sources (
            entry_id, source_id, source_kind, source_node_id, language, entry_kind, dialect,
            dialect_scope, surface, normalized_surface, normalization_version, source_revision,
            is_deleted, content_scope, publication_id, pos_id, pos, form_type,
            content_schema_version, form_id, variant_id, group_ids, projection_version
        )
        SELECT base.entry_id, base.source_id || ':bulk:' || bulk.n, base.source_kind,
               base.source_node_id, base.language, base.entry_kind, base.dialect,
               base.dialect_scope, base.surface, base.normalized_surface,
               base.normalization_version, base.source_revision, base.is_deleted,
               base.content_scope, base.publication_id, base.pos_id, base.pos, base.form_type,
               base.content_schema_version, base.form_id, base.variant_id, base.group_ids,
               base.projection_version
        FROM (
            SELECT * FROM lexicon.surface_sources
            WHERE entry_id = $1
              AND content_scope = 'current_publication'
              AND is_deleted = FALSE
              AND pos_id IS NOT NULL
              AND surface ILIKE '%harbour%'
            ORDER BY source_id
            LIMIT 1
        ) base, generate_series(1, 2000) AS bulk(n)
        "#,
    )
    .bind(entry_id)
    .execute(&pool)
    .await
    .expect("应能把词面行复制到扫描上限");
    assert_eq!(cloned.rows_affected(), 2000);

    let (status, capped) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbour", "page_size": 200}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{capped}");
    assert_eq!(
        capped["truncated"], true,
        "取回行数触顶必须标 truncated：{capped}"
    );
    let matches = capped["matches"].as_array().unwrap();
    assert!(
        matches.len() < 200,
        "触顶与「超出 page_size」是两条独立成因，去重后候选仍可能远少于一页：{capped}"
    );
    assert!(
        matches
            .iter()
            .any(|candidate| candidate["entry_id"] == json!(entry_id)),
        "触顶不该把命中的词条整个丢掉：{capped}"
    );
}

#[sqlx::test]
async fn component_target_search_ranks_exact_before_prefix_before_contains_and_pages_with_a_cursor(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 字典序是 big harbour < harbour < harbour club，档位却是 harbour（等于）< harbour club
    // （前缀）< big harbour（包含）。只按字典序排的话，点 harbour 先看到的是 big harbour。
    let word_draft =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["The harbour is calm."])
            .await;
    let (status, word_published) = publish_ready_v3(&state, &bearer, &word_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{word_published}");
    let exact_id = word_published["word"]["id"].as_str().unwrap().to_owned();
    let (prefix_published, _) =
        create_published_v3_phrase(&state, &pool, &bearer, "harbour club", json!([])).await;
    let prefix_id = prefix_published["word"]["id"].as_str().unwrap().to_owned();
    let (contains_published, _) =
        create_published_v3_phrase(&state, &pool, &bearer, "big harbour", json!([])).await;
    let contains_id = contains_published["word"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let entry_sequence = |response: &Value| -> Vec<String> {
        response["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|candidate| candidate["entry_id"].as_str().unwrap().to_owned())
            .collect()
    };
    let distinct_in_order = |sequence: &[String]| -> Vec<String> {
        let mut seen = HashSet::new();
        sequence
            .iter()
            .filter(|id| seen.insert((*id).clone()))
            .cloned()
            .collect()
    };

    let (status, whole) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbour", "page_size": 200}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{whole}");
    let whole_sequence = entry_sequence(&whole);
    assert_eq!(
        distinct_in_order(&whole_sequence),
        vec![exact_id.clone(), prefix_id.clone(), contains_id.clone()],
        "等于 → 前缀 → 包含，而不是字典序：{whole}"
    );
    assert!(
        whole.get("next_cursor").is_none(),
        "一页装得下就不该有下一页：{whole}"
    );
    assert_eq!(whole["truncated"], false);
    assert!(
        whole_sequence.len() > 3,
        "三个词条各自展开多条候选，才能让逐条翻页有意义：{whole}"
    );

    // 逐条翻页：每页 1 条，拼起来必须与整页的顺序逐条相同，total 全程不变。
    let mut cursor: Option<String> = None;
    let mut walked = Vec::new();
    for _ in 0..200 {
        let mut body = json!({"schema_version": 3, "q": "harbour", "page_size": 1});
        if let Some(cursor) = &cursor {
            body["cursor"] = json!(cursor);
        }
        let (status, page) = search_component_targets(&state, &bearer, body).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(page["total"], whole["total"], "{page}");
        let mut ids = entry_sequence(&page);
        assert_eq!(ids.len(), 1, "{page}");
        walked.append(&mut ids);
        match page["next_cursor"].as_str() {
            Some(next) => {
                assert_eq!(page["truncated"], true, "有下一页必须标 truncated：{page}");
                cursor = Some(next.to_owned());
            }
            None => {
                assert_eq!(page["truncated"], false, "最后一页不该标 truncated：{page}");
                break;
            }
        }
    }
    assert_eq!(walked, whole_sequence, "翻页拼接必须与整页逐条一致");

    // 游标绑定 q 与 kind；词库一变（generation 前进）旧游标即失效。
    let (status, first) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbour", "page_size": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    for body in [
        json!({"schema_version": 3, "q": "harbour club", "page_size": 1, "cursor": cursor}),
        json!({"schema_version": 3, "q": "harbour", "kind": "phrase", "page_size": 1, "cursor": cursor}),
        json!({"schema_version": 3, "q": "harbour", "page_size": 1, "cursor": "garbage"}),
    ] {
        let (status, rejected) = search_component_targets(&state, &bearer, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} 应被拒：{rejected}");
        assert_eq!(rejected["code"], "invalid_query", "{rejected}");
        assert_eq!(rejected["field"], "cursor", "{rejected}");
    }
    sqlx::query(
        "UPDATE lexicon.sentence_discovery_generation SET generation = generation + 1 WHERE singleton = TRUE",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (status, stale) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbour", "page_size": 1, "cursor": cursor}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "词库变动后旧游标必须失效：{stale}"
    );
    assert_eq!(stale["field"], "cursor", "{stale}");
}
