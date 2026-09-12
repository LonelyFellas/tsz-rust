//! 智能词库从检测、建稿、分步保存到不可变发布的主链路契约测试。

use std::collections::HashSet;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    config::SmartLexiconV3Flags,
    lexicon::dto::SurfacePolicyNameV2,
    lexicon::normalization::HEADWORD_NORMALIZATION_VERSION,
    lexicon::validation::MAX_STEP_CONTENT_BODY_BYTES,
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

/// 单词性、单原形的最小 V3 词形内容，词面可指定。
fn v3_forms_fixture_for(surface: &str) -> Value {
    let form_id = Uuid::now_v7();
    json!({
        "pos": [{
            "pos_id": Uuid::now_v7(),
            "pos": "noun",
            // 两侧同拼，所以 spelling_mode 是 unified；写成 distinguish 只是恰好被放行，
            // 会让后来者照抄出现实中不存在的形状。
            "dialect_rules": {
                "spelling_mode": "unified",
                "phonetic_mode": "distinguish"
            },
            "forms": [{
                "id": form_id,
                "form_type": "base",
                "regional_variants": {
                    "mode": "uk_us",
                    "uk": {
                        "id": Uuid::now_v7(),
                        "dialect": "uk",
                        "spelling": surface,
                        "origin": "manual",
                        "pronunciations": [{
                            "id": Uuid::now_v7(),
                            "dict_phonetic": "/test/",
                            "actual_pron": "test",
                            "style": "normal"
                        }]
                    },
                    "us": {
                        "id": Uuid::now_v7(),
                        "dialect": "us",
                        "spelling": surface,
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
                "id": Uuid::now_v7(),
                "is_regular": true,
                "members": [{"id": Uuid::now_v7(), "form_id": form_id}]
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
    create_v3_forms_fixture(state, pool, bearer, false).await
}

// Explicit opt-in for pre-existing tests whose fixture intentionally creates homonyms.
async fn create_v3_with_annotated_complete_forms(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
) -> Value {
    create_v3_forms_fixture(state, pool, bearer, true).await
}

async fn create_v3_forms_fixture(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
    annotate: bool,
) -> Value {
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
    let (status, created) = if annotate {
        create_annotated_fixture(state, bearer, Uuid::now_v7(), create_input).await
    } else {
        call(
            state,
            Method::POST,
            &format!("{ROOT}/entries"),
            bearer,
            Some(Uuid::now_v7()),
            Some(create_input),
        )
        .await
    };
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

fn first_sentence(word: &Value) -> &Value {
    &word["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]
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
async fn v3_sense_group_voice_editor(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let word =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["A harbour sentence."])
            .await;
    assert!(
        word["word"]["meanings"]["sense_groups"][0]
            .get("name_en_rich")
            .is_none()
    );
    let mut meanings = word["word"]["meanings"].clone();
    let text = meanings["sense_groups"][0]["name_en"]
        .as_str()
        .unwrap()
        .to_owned();
    let rich = json!({"version":2,"text":text,"annotations":[]});
    let profile = json!({"voices":[{"voice_id":"sonia","enabled":true,"rate_percent":10}]});
    meanings["sense_groups"][0]["name_en_rich"] = rich.clone();
    meanings["sense_groups"][0]["voice_profile"] = profile.clone();
    let saved = save_v3_meanings(&state, &bearer, &word, meanings).await;
    let assert_fields = |body: &Value| {
        assert_eq!(
            body["word"]["meanings"]["sense_groups"][0]["name_en_rich"],
            rich
        );
        assert_eq!(
            body["word"]["meanings"]["sense_groups"][0]["voice_profile"],
            profile
        );
    };
    assert_fields(&saved);
    let (status, fetched) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{}", word["word"]["id"].as_str().unwrap()),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fetched}");
    assert_fields(&fetched);
    let (status, published) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_fields(&published);
}

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

    // A complete catalog exceeds the former 20-item limit; also cover the new boundary.
    let grammar_profile = json!({"voices": (0..2000).map(|index| json!({"voice_id": format!("voice-{index}"), "enabled": index % 2 == 0, "rate_percent": if index % 2 == 0 { -25 } else { 10 }})).collect::<Vec<_>>()});
    let sentence_profile =
        json!({"voices": [{"voice_id":"jenny", "enabled":true, "rate_percent":20}]});
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
        json!({"voices": [{"voice_id":"sonia","enabled":true,"rate_percent":101}]}),
        json!({"voices": [{"voice_id":"sonia","enabled":true,"rate_percent":-51}]}),
        json!({"voices": [{"voice_id":"sonia","enabled":true,"rate_percent":0},{"voice_id":"sonia","enabled":false,"rate_percent":10}]}),
        json!({"voices": [{"voice_id":"","enabled":false,"rate_percent":0}]}),
        json!({"voices": (0..2001).map(|index| json!({"voice_id":format!("v{index}"),"enabled":false,"rate_percent":0})).collect::<Vec<_>>()}),
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
    meanings["pos"][0]["grammar_structures"][0]["variants"][0]["voice_profile"] = json!({"voices": [{"voice_id":"a-voice-that-no-longer-exists","enabled":false,"rate_percent":100}]});
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
async fn v3_draft_without_any_form_stays_listed(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    // 内置词典没收录的短语：建出来只有管理员确认过的词面摘要，没有任何词性与词形。
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
            "surface": "a piece of cake"
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
            "kind": "phrase",
            "headwords": { "mode": "unified", "common": "a piece of cake" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let entry_id = created["word"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        created["word"]["forms"]["pos"],
        json!([]),
        "词典没收录时不应凭空补词性：{created}"
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
    assert_eq!(
        list["page"]["total"], 1,
        "还没填词形的在途草稿必须留在列表里，否则管理员建完就找不回：{list}"
    );
    assert_eq!(list["words"][0]["id"], json!(entry_id), "{list}");
    assert_eq!(
        list["words"][0]["presentation"]["label"],
        json!("a piece of cake"),
        "列表按管理员确认过的词面显示：{list}"
    );

    // 翻到空页时总数走的是另一条 count 查询，口径必须与主查询一致。
    let (status, empty_page) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?page=9&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{empty_page}");
    assert_eq!(empty_page["words"], json!([]), "{empty_page}");
    assert_eq!(
        empty_page["page"]["total"], list["page"]["total"],
        "空页回退的总数要与主查询一致：{empty_page}"
    );
}

#[sqlx::test]
async fn related_search_matches_whole_words_only(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    for surface in ["a piece of cake", "apple pie"] {
        let (entry_id, forms) = create_v3_phrase_draft(&state, &bearer, surface).await;
        save_v3_forms_after_impact(&state, &bearer, &entry_id, 1, "save", forms).await;
    }

    let search = |q: &str| {
        let path = format!(
            "{ROOT}/entries/related-search?q={}&page_size=20&include_drafts=true",
            q.replace(' ', "%20")
        );
        let bearer = bearer.clone();
        let state = &state;
        async move {
            let (status, body) = call(state, Method::GET, &path, &bearer, None, None).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let mut labels = body["results"]
                .as_array()
                .expect("结果必须是数组")
                .iter()
                .map(|item| {
                    item["presentation"]["label"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect::<Vec<_>>();
            labels.sort();
            labels
        }
    };

    // 半截拼写不算词：ap 既不是 apple 也不是 pie。
    assert!(search("ap").await.is_empty(), "半截拼写不该命中任何词条");
    // 完整单词命中含它的短语。
    assert_eq!(search("apple").await, vec!["apple pie".to_owned()]);
    // 短语里的独立单词同样算数。
    assert_eq!(search("a").await, vec!["a piece of cake".to_owned()]);
    // 关键词本身是短语时按整条词面比，否则它反而搜不到自己。
    assert_eq!(
        search("a piece of cake").await,
        vec!["a piece of cake".to_owned()]
    );
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
async fn v3_text_relation_round_trips_through_publication(pool: PgPool) {
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

    let (status, validation) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/validate"),
        &bearer,
        None,
        Some(json!({"schema_version": 3, "base_revision": saved["word"]["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{validation}");
    assert_eq!(
        validation["valid"], true,
        "文本关联不应阻断发布校验：{validation}"
    );

    // 纯文本关联允许发布，且不得自动创建或绑定目标词条。
    let (status, published) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "文本关联应允许发布：{published}"
    );
    let published_relation = &published["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(published_relation, saved_relation);
    let snapshot_relation: Value = sqlx::query_scalar(
        "SELECT snapshot->'meanings'->'pos'->0->'senses'->0->'relations'->0 FROM lexicon.entry_publications WHERE entry_id = $1",
    )
    .bind(Uuid::parse_str(entry_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(&snapshot_relation, saved_relation);

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
    assert!(reloaded_relation["target_word_id"].is_null());
    assert_eq!(
        reloaded_relation["pending_target_headword"],
        pending_headword
    );
    assert_eq!(reloaded_relation["pending_target_gloss"], pending_gloss);

    // 纯文本关联词绝不物化成词条：按 surface 投影查，V2 的 entry_headword_keys 已随格式下线。
    let materialized_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT entry_id FROM lexicon.surface_sources WHERE normalized_surface = $1 LIMIT 1",
    )
    .bind(&pending_headword)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(
        materialized_id.is_none(),
        "text must never create a target entry"
    );
}

#[sqlx::test]
async fn v3_freetext_relation_saves_and_publishes(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let forms_saved = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let entry_id = forms_saved["word"]["id"].as_str().unwrap();
    let relation_id = Uuid::now_v7();
    let mut meanings =
        complete_v3_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone());
    // 手输的待建词面就是一段普通文本，草稿期不该拿「合法英文词条名」去卡它。
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id,
        "relation": "derivative",
        "pending_target_headword": "暂记：回头查这个词",
        "score": "0.00"
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
            "intent": "save",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "手输文本不该挡住草稿保存：{saved}");
    let saved_relation = &saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(
        saved_relation["pending_target_headword"],
        "暂记：回头查这个词"
    );
    assert!(saved_relation["target_word_id"].is_null());

    let (status, published) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "自由文本关联应允许发布：{published}"
    );
    assert_eq!(
        published["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0],
        *saved_relation
    );
}

/// 关联词候选对**所有**管理员亮出草稿（2026-09-11 口径）：草稿能被别人引用。
///
/// 此前这里断言的是相反的事：外人搜别人的草稿只得到空结果。放开后关联词搜索与撞名
/// 机器一致，看得见也绑得上；写权限不受影响，仍由 ensure_draft_writable 守着。
/// 例句发现（sentence-targets/resolve）不在本次放开范围，草稿候选仍只对创建者可见。
#[sqlx::test]
async fn relation_draft_candidates_open_up_while_discovery_stays_creator_only(pool: PgPool) {
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
    let outsider_results = outsider_search["results"].as_array().unwrap();
    assert_eq!(
        outsider_results.len(),
        1,
        "别人的未发布草稿也应进入关联词候选：{outsider_search}"
    );
    assert_eq!(outsider_results[0]["entry_id"], owner_entry_id);
    assert_eq!(
        outsider_results[0]["status"], "draft",
        "跨创建者候选仍应标成草稿：{outsider_search}"
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

    // 例句发现的草稿候选不跟着放开：别人的未发布草稿仍不可见。
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

/// 别人的草稿不只是能被搜到，还要真能绑成关联词并跟着发布。
///
/// 搜索侧由 `relation_draft_candidates_open_up_while_discovery_stays_creator_only` 守着；
/// 这条守写入与发布侧：写入面没有 creator 谓词是「可引用」成立的前提，一旦有人给
/// `resolve_relation_targets` 加上过滤，功能会静默失效而搜索那条测试照常绿。
#[sqlx::test]
async fn an_outsider_binds_and_publishes_a_relation_to_another_admins_draft_sense(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let owner = token(&state, seed_admin(&pool).await);
    let outsider = token(&state, seed_admin(&pool).await);

    // owner 的草稿：存了词义，但始终不发布。
    let target = create_v3_with_complete_forms(&state, &pool, &owner).await;
    let target_id = target["word"]["id"].as_str().unwrap().to_owned();
    let mut target_content =
        complete_v3_meanings_fixture(target["word"]["forms"]["pos"][0]["pos_id"].clone());
    // source 自己的释义同样来自这个夹具（「港口」），目标释义必须换成独有串，
    // 否则回填断言无法自证读的是别人的草稿。
    target_content["pos"][0]["senses"][0]["definitions"][0]["content"] =
        rich_text("别人草稿的港口");
    let target_sense_id = target_content["pos"][0]["senses"][0]["id"].clone();
    save_v3_meanings(&state, &owner, &target, target_content).await;

    // outsider 另起一个词面建词条：同形词要走标注确认，而标注只有创建者能改，
    // 会把这条用例卡在与本次改动无关的那道门上。
    let source_id = create_legacy_v3_empty_skeleton(&state, &outsider, "wharf").await;
    let (_, source) = save_v3_forms_after_impact(
        &state,
        &outsider,
        &source_id.to_string(),
        1,
        "complete",
        v3_forms_fixture_for("wharf"),
    )
    .await;
    let relation_id = Uuid::now_v7();
    let mut content =
        complete_v3_meanings_fixture(source["word"]["forms"]["pos"][0]["pos_id"].clone());
    content["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id,
        "relation": "derivative",
        "score": "80.00",
        "target_word_id": target_id,
        "target_sense_id": target_sense_id
    }]);
    let saved = save_v3_meanings(&state, &outsider, &source, content).await;
    let relation = &saved["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(
        relation["target_sense_id"], target_sense_id,
        "别人的草稿词义应能被绑定：{saved}"
    );
    assert_eq!(
        relation["target_status"], "draft",
        "目标仍是草稿，状态要如实标出：{saved}"
    );
    assert_eq!(
        relation["target_gloss"], "别人草稿的港口",
        "服务端应回填目标草稿的词义快照：{saved}"
    );

    // 发布引用方：引用落在草稿作用域，且不带发布号。
    let (status, published) = publish_ready_v3(&state, &outsider, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    let (scope, publication_id) = sqlx::query_as::<_, (String, Option<Uuid>)>(
        "SELECT target_content_scope::text, target_publication_id
         FROM lexicon.entry_publication_sense_refs
         WHERE source_node_id = $1
           AND entry_id = $2
           AND reference_kind = 'relation'",
    )
    .bind(relation_id)
    .bind(Uuid::parse_str(saved["word"]["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .expect("发布后应留下一条指向草稿词义的引用行");
    assert_eq!(scope, "draft");
    assert_eq!(publication_id, None);
}

/// 撞名机器对**所有**管理员亮出草稿命中（2026-09-08 口径）。
///
/// 此前这里断言的是相反的事：别人的草稿被过滤成空，外人不但看不见，还会静默建出
/// 第二份同名草稿。放开后撞名回到软确认治理——看得见、要 acknowledge、确认后共存。
/// 写权限不受影响，仍由 ensure_draft_writable 守着。
#[sqlx::test]
async fn surface_machinery_shows_other_admins_drafts(pool: PgPool) {
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

    // 检测：别人的未发布草稿现在会亮进 surface warning。
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
        outsider_detection["requires_acknowledgement"], true,
        "别人的草稿同样构成检测确认前提：{outsider_detection}"
    );
    let outsider_items = outsider_detection["surface_match_page"]["items"]
        .as_array()
        .expect("外人应看到别人草稿的命中");
    assert!(
        outsider_items
            .iter()
            .all(|item| item["match"]["entry_id"] == owner_entry_id),
        "命中应指向 owner 的草稿：{outsider_detection}"
    );

    // 创建者自己检测照旧（防止把过滤改成「谁都看不见」而全绿）。
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

    // 建档：撞名不是硬拦，acknowledge 之后同名草稿仍可共存。
    let mut create_input = json!({
        "schema_version": 3,
        "detection_id": outsider_detection["detection_id"],
        "kind": "word",
        "annotation": "1"
    });
    if let Some(confirm) =
        outsider_detection["surface_match_page"]["surface_confirmation_token"].as_str()
    {
        create_input["confirmed_surface_match_token"] = json!(confirm);
    }
    let (status, outsider_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &outsider,
        Some(Uuid::now_v7()),
        Some(create_input),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "确认撞名后应允许共存：{outsider_created}"
    );
    let outsider_entry_id = outsider_created["word"]["id"].as_str().unwrap().to_string();

    // 词形步：impact 预览会亮出别人的草稿词形，保存需带确认。
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
        !impact["surface_match_page"].is_null(),
        "词形步应亮出别人的草稿词形：{impact}"
    );
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
    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{outsider_entry_id}/steps/forms"),
        &outsider,
        None,
        Some(forms_input),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "确认撞名后词形应能保存：{saved}");

    // 可见性是双向的：owner 现在也看得到 outsider 的草稿。
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
        .expect("创建者应看到命中");
    assert!(
        owner_items
            .iter()
            .any(|item| item["match"]["entry_id"] == outsider_entry_id.as_str()),
        "对方的草稿对 owner 同样可见：{owner_redetection}"
    );
}

#[sqlx::test]
async fn other_admins_drafts_require_acknowledgement_and_then_coexist(pool: PgPool) {
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

    // outsider 全链手动走。每一步都刻意先不带 surface token，用「被拒 → 带 token 重来」
    // 证明别人的草稿真的进了重算集合——若直接用会自动 acknowledge 的
    // create_v3_with_complete_forms，过滤退回旧口径时本测试照样绿，就没有判别力了。
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
        !outsider_detection["surface_match_page"].is_null(),
        "检测应亮出 owner 的草稿：{outsider_detection}"
    );
    let mut create_input = json!({
        "schema_version": 3,
        "detection_id": outsider_detection["detection_id"],
        "kind": "word",
        "annotation": "1"
    });
    if let Some(token) =
        outsider_detection["surface_match_page"]["surface_confirmation_token"].as_str()
    {
        create_input["confirmed_surface_match_token"] = json!(token);
    }
    let (status, outsider_created) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &outsider,
        Some(Uuid::now_v7()),
        Some(create_input),
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
        !impact["surface_match_page"].is_null(),
        "词形步应亮出 owner 的草稿词形：{impact}"
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
    if let Some(token) = impact["surface_match_page"]["impact_confirmation_token"].as_str() {
        forms_input["confirmed_impact_token"] = json!(token);
    }

    // 关键判别：词形步重算的命中集合含 owner 的草稿，不确认就存不进去。
    // （确认过一次之后发布期不再重复要求，所以判别力必须落在这一步。）
    let (status, blocked_forms) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{outsider_entry_id}/steps/forms"),
        &outsider,
        None,
        Some(forms_input.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "撞上 owner 的草稿词形，保存必须先确认：{blocked_forms}"
    );
    assert_eq!(
        blocked_forms["code"], "surface_match_acknowledgement_required",
        "{blocked_forms}"
    );
    assert!(
        blocked_forms["meta"]["surface_match_page"]["surface_confirmation_token"].is_string(),
        "确认页应签发 surface token：{blocked_forms}"
    );
    // 被拒的那次让预览快照作废，重取一份——surface 与 impact 两张票必须同源，
    // 混用新旧会换来一个 410 surface_match_snapshot_expired。
    let (status, retry_impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{outsider_entry_id}/steps/forms/impact"),
        &outsider,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": 1,
            "content": forms_input["content"].clone()
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{retry_impact}");
    if let Some(token) = retry_impact["confirmation_token"].as_str() {
        forms_input["confirmed_impact_token"] = json!(token);
    }
    if let Some(token) = retry_impact["surface_match_page"]["impact_confirmation_token"].as_str() {
        forms_input["confirmed_impact_token"] = json!(token);
    }
    if let Some(token) = retry_impact["surface_match_page"]["surface_confirmation_token"].as_str() {
        forms_input["confirmed_surface_match_token"] = json!(token);
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
        "确认后词形应能保存：{outsider_forms}"
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

    // 词形步已确认过这批命中，发布期不再重复要求——同名词条就此共存。
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
        "确认后同名词条应可共存发布：{outsider_published}"
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
    // content_scope 是行级判别（status 是词条级 lifecycle，两者不可互替）。
    // 放开后 outsider 词条的已发布行与其草稿工作区行都会亮给 owner——这里同时钉住
    // 两种 scope 都在，以及命中确实指向 outsider 那条词条而非张冠李戴。
    let owner_items = owner_warning["meta"]["surface_match_page"]["items"]
        .as_array()
        .expect("发布警告应带命中项");
    assert!(
        owner_items
            .iter()
            .all(|item| item["match"]["entry_id"] == outsider_entry_id),
        "命中应全部指向 outsider 的词条：{owner_warning}"
    );
    for scope in ["current_publication", "draft"] {
        assert!(
            owner_items
                .iter()
                .any(|item| item["match"]["content_scope"] == scope),
            "发布警告应亮出 {scope} 行：{owner_warning}"
        );
    }
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

/// 入站关系预览对所有管理员亮出草稿来源（2026-09-08 口径）。
///
/// 末段的「发布后两边各恰一条」是去重条件的守护断言：草稿分支放开后，发布分支
/// 仍要在存在对应草稿行时让位，既不能双出也不能双失明。
#[sqlx::test]
async fn inbound_relation_previews_show_other_admins_draft_sources(pool: PgPool) {
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

    // 外人检测命中 P：入站预览同样亮出 referrer 的未发布草稿。
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
    assert!(
        outsider_context["inbound_relations"]["previews"]
            .as_array()
            .is_some_and(|previews| previews.iter().any(|preview| {
                preview["source_entry_id"] == referrer_entry_id
                    && preview["source_status"] == "draft"
            })),
        "别人的草稿引用同样进入站预览：{outsider_context}"
    );

    // 创建者自己检测：看到的与外人一致（防止改成「谁都看不见」而全绿）。
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

    let target_forms = create_v3_with_annotated_complete_forms(&state, &pool, &bearer).await;
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

    let source_forms = create_v3_with_annotated_complete_forms(&state, &pool, &bearer).await;
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
    let (status, second_created) =
        create_annotated_fixture(&state, &bearer, Uuid::now_v7(), confirmed).await;
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

    seed_phrase_noun(&pool).await;
    let mut forms = phrase_forms_fixture("native phrase");
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
            "content": phrase_meanings_fixture(pos_id)
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
    seed_phrase_noun(&state.pool).await;
    (entry_id, phrase_forms_fixture(surface))
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
                phrase_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone())
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

    let mut cycle_forms = phrase_forms_fixture("double phrase");
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
    outer_forms["pos"][0]["pos"] = json!(PHRASE_NOUN);
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
            "content": phrase_meanings_fixture(
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
    let mut third_forms = phrase_forms_fixture("third phrase");
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
        phrase_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone());
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
async fn v3_publish_with_bound_relations_keeps_sense_phrase_components(pool: PgPool) {
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

    // 已绑定关联词发布应保留释义级成分；纯文本关联另有发布回归覆盖。
    let mut meanings = saved["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": target_published["word"]["id"],
        "target_sense_id": target_published["word"]["meanings"]["pos"][0]["senses"][0]["id"],
        "score": "88.00"
    }]);
    let with_pending = save_v3_meanings(&state, &bearer, &saved, meanings).await;

    let (status, published) = publish_ready_v3(&state, &bearer, &with_pending).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "带绑定关联词的发布必须成功：{published}"
    );
    assert_eq!(
        published["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"][0]["id"],
        component["id"],
        "发布不得吞掉释义级成分：{published}"
    );
    assert_eq!(
        published["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_word_id"],
        target_published["word"]["id"],
        "绑定关联词应保留目标：{published}"
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
    assert_eq!(published_component_nodes, 1, "发布必须保留成分节点");
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
    let base_meanings = phrase_meanings_fixture(victim_pos_id.clone());
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
        phrase_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone());

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
        "sub_pos": "",
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
        capabilities["draft_relation_prebinding"], false,
        "既有能力位仍旧无条件输出"
    );

    let mut meanings =
        phrase_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone());
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
async fn v3_surface_warning_tokens_bind_actor_command_revision_digest_and_policy(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    // 「换个人用同一个 token」这一断言要走超管：普通管理员会先撞上草稿归属守卫
    // （403 entry_edit_forbidden，见 draft_writes_are_restricted_to_their_creator_unless_super_admin），
    // 根本到不了 token 校验。超管豁免归属、但不豁免 token 的 actor 绑定——正好把这条钉住。
    let other_admin_id = seed_admin_with_role(&pool, AdminRole::SuperAdmin).await;
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
    let (status, second_created) =
        create_annotated_fixture(&state, &bearer, duplicate_key, confirmed_create).await;
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
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "smart_lexicon_v3_storage_unavailable");

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
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "smart_lexicon_v3_storage_unavailable");

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
    unknown_form_type["content"]["pos"][0]["forms"][0]["form_type"] = json!("invalid-form-type");
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
        .expect("非法词形编码应返回稳定 V3 issue");
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

async fn create_ready_v3_draft_with_sentences(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
    texts: &[&str],
) -> Value {
    create_ready_v3_sentences_fixture(state, pool, bearer, texts, false).await
}

async fn create_ready_v3_annotated_draft_with_sentences(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
    texts: &[&str],
) -> Value {
    create_ready_v3_sentences_fixture(state, pool, bearer, texts, true).await
}

async fn create_ready_v3_sentences_fixture(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
    texts: &[&str],
    annotate: bool,
) -> Value {
    let forms_saved = create_v3_forms_fixture(state, pool, bearer, annotate).await;
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
    let stored_roles: (String, String, bool, String) = sqlx::query_as(
        r#"
        SELECT node.node_role, translation.field_role, node.stable_slot, translation.language
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
            "meanings.zh_translation".to_owned(),
            "zh_translation_balanced_fluency".to_owned(),
            false,
            "zh".to_owned()
        )
    );
}

/// 发布路径的 V2 往返曾把每句多档 zh_translations 塌成 1 档（既有缺陷）。
/// 钉住：发布响应与不可变快照都保留全部三档；带纯文本关联的发布同样保留。
#[sqlx::test]
async fn v3_publish_preserves_all_sentence_translation_bands(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);

    let word = create_ready_v3_annotated_draft_with_sentences(
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

    // 存三档译文（乱序给，落库后按初、中、高排）。
    let mut meanings = word["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["sentences"][0]["zh_translations"] = json!([
        {"id": Uuid::now_v7(), "band": "adapted_creation", "content": rich_text("高阶译文")},
        {"id": Uuid::now_v7(), "band": "word_for_word", "content": rich_text("初阶译文")},
        {"id": b_id, "band": "balanced_fluency", "content": rich_text("中阶译文")}
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
        ["word_for_word", "balanced_fluency", "adapted_creation"],
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
        ["word_for_word", "balanced_fluency", "adapted_creation"],
        "发布快照必须固化全部三档，否则下游只能读到 1 档"
    );

    // 3) 带纯文本关联的发布同样保留三档
    let source = create_ready_v3_annotated_draft_with_sentences(
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
    let mut src_meanings = source["word"]["meanings"].clone();
    src_meanings["pos"][0]["senses"][0]["sentences"][0]["zh_translations"] = json!([
        {"id": Uuid::now_v7(), "band": "adapted_creation", "content": rich_text("源高阶")},
        {"id": src_sentence_b_id, "band": "balanced_fluency", "content": rich_text("源中阶")},
        {"id": Uuid::now_v7(), "band": "word_for_word", "content": rich_text("源初阶")}
    ]);
    // 纯文本关联无需绑定词条即可随内容发布。
    src_meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "pending_target_headword": "handwritten relation",
        "score": "88.00"
    }]);
    let src_saved = save_v3_meanings(&state, &bearer, &source, src_meanings).await;
    let (status, src_published) = publish_ready_v3(&state, &bearer, &src_saved).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "带纯文本关联词的发布必须成功：{src_published}"
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
        ["word_for_word", "balanced_fluency", "adapted_creation"],
        "带关联词发布必须保留三档：{src_published}"
    );
    assert_eq!(
        src_published["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["pending_target_headword"],
        "handwritten relation",
        "纯文本关联应保留文本：{src_published}"
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
        "纯文本关联发布后投影必须保留三档"
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
    assert_eq!(initial[0]["band"], "balanced_fluency");

    let c_id = Uuid::now_v7();
    let b_id = initial[0]["id"].as_str().unwrap().to_owned();
    let a_id = Uuid::now_v7();
    let mut meanings = word["word"]["meanings"].clone();
    let sentence = &mut meanings["pos"][0]["senses"][0]["sentences"][0];
    // 首档别名那条显式带 language（走 UPDATE 路径），另两档省略（走 INSERT 路径），
    // 两条路径都必须把 zh 写进 text_variants.language。
    sentence["zh_translations"] = json!([
        {"id": a_id, "band": "adapted_creation", "content": rich_text("高阶译文")},
        {"id": c_id, "band": "word_for_word", "content": rich_text("初阶译文")},
        {"id": b_id, "band": "balanced_fluency", "content": rich_text("中阶译文"), "language": "zh"}
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
        ["word_for_word", "balanced_fluency", "adapted_creation"]
    );
    assert!(
        translations
            .iter()
            .all(|translation| translation["language"] == "zh"),
        "请求缺省 language 的那几档，响应也要补成汉语：{saved}"
    );
    assert_eq!(first_sentence(&saved)["zh_text_id"], b_id);
    assert_eq!(first_sentence(&saved)["zh_text"]["text"], "中阶译文");

    let stored: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        r#"
        SELECT id, field_role, plain_text, language
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
            .map(|(_, role, text, language)| (role.as_str(), text.as_str(), language.as_str()))
            .collect::<Vec<_>>(),
        [
            ("zh_translation_word_for_word", "初阶译文", "zh"),
            ("zh_translation_balanced_fluency", "中阶译文", "zh"),
            ("zh_translation_adapted_creation", "高阶译文", "zh"),
        ]
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

    // 例句难度等级与译文风格无关：改 level 不该把兼容字段指向另一档译文。
    let mut changed_level_meanings = reloaded["word"]["meanings"].clone();
    changed_level_meanings["pos"][0]["senses"][0]["sentences"][0]["level"] = json!("A1");
    let changed_level = save_v3_meanings(&state, &bearer, &reloaded, changed_level_meanings).await;
    assert_eq!(first_sentence(&changed_level)["zh_text_id"], b_id);
    assert_eq!(
        first_sentence(&changed_level)["zh_text"]["text"],
        "中阶译文"
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
    assert_eq!(cleared_translations[0]["id"], b_id);
    assert_eq!(cleared_translations[0]["content"]["text"], "中阶译文");
}

#[sqlx::test]
async fn v3_sentence_translations_allow_repeated_bands_and_independent_edits(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let word = create_ready_v3_annotated_draft_with_sentences(
        &state,
        &pool,
        &bearer,
        &["Multiple translations for one sentence."],
    )
    .await;
    let entry_id = word["word"]["id"].as_str().unwrap();
    let mut meanings = writable_v3_meanings(&word);
    let rows: Vec<Value> = ["adapted_creation", "balanced_fluency", "word_for_word"].into_iter().flat_map(|band| {
        (1..=2).map(move |index| json!({"id":Uuid::now_v7(),"band":band,"content":rich_text(&format!("{band} 译文 {index}"))}))
    }).collect();
    meanings["pos"][0]["senses"][0]["sentences"][0]["zh_translations"] = json!(rows);
    let saved = save_v3_meanings(&state, &bearer, &word, meanings).await;
    let expected = first_sentence(&saved)["zh_translations"].clone();
    assert_eq!(expected.as_array().unwrap().len(), 6);
    let stored: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, plain_text, language FROM lexicon.text_variants WHERE owner_node_id = $1::uuid AND field_role LIKE 'zh_translation_%' ORDER BY sort_order"
    ).bind(first_sentence(&saved)["id"].as_str().unwrap()).fetch_all(&pool).await.unwrap();
    assert_eq!(
        stored,
        expected
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (
                Uuid::parse_str(t["id"].as_str().unwrap()).unwrap(),
                t["content"]["text"].as_str().unwrap().to_owned(),
                t["language"].as_str().unwrap().to_owned()
            ))
            .collect::<Vec<_>>(),
        "六条译文的语言列都要跟 wire 上的 language 一致"
    );
    let (status, published) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_eq!(first_sentence(&published)["zh_translations"], expected);
    let publication_id = current_publication_id(&pool, Uuid::parse_str(entry_id).unwrap()).await;
    let snapshot: Value =
        sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id = $1")
            .bind(publication_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        snapshot["meanings"]["pos"][0]["senses"][0]["sentences"][0]["zh_translations"],
        expected
    );

    // 同一个 ID 可改档；删除当前别名后由剩余译文重新产生兼容字段。
    let alias_id = first_sentence(&published)["zh_text_id"].clone();
    let mut edited = writable_v3_meanings(&published);
    let translations = edited["pos"][0]["senses"][0]["sentences"][0]["zh_translations"]
        .as_array_mut()
        .unwrap();
    translations.retain(|t| t["id"] != alias_id);
    let changed_id = translations[0]["id"].clone();
    translations[0]["band"] = json!("adapted_creation");
    translations[0]["content"] = rich_text("独立修改并改成高阶");
    let changed = save_v3_meanings(&state, &bearer, &published, edited).await;
    let after = first_sentence(&changed)["zh_translations"]
        .as_array()
        .unwrap();
    assert_eq!(after.len(), 5);
    assert!(!after.iter().any(|t| t["id"] == alias_id));
    assert_eq!(
        after.iter().find(|t| t["id"] == changed_id).unwrap()["content"]["text"],
        "独立修改并改成高阶"
    );
    assert_ne!(first_sentence(&changed)["zh_text_id"], alias_id);
    for translation in after.iter().filter(|t| t["id"] != changed_id) {
        assert!(expected.as_array().unwrap().contains(translation));
    }
    let (status, reloaded) = call(
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
        first_sentence(&reloaded)["zh_translations"],
        first_sentence(&changed)["zh_translations"]
    );
    let mut missing = writable_v3_meanings(&reloaded);
    missing["pos"][0]["senses"][0]["sentences"][0]
        .as_object_mut()
        .unwrap()
        .remove("zh_translations");
    let preserved = save_v3_meanings(&state, &bearer, &reloaded, missing).await;
    assert_eq!(
        first_sentence(&preserved)["zh_translations"],
        first_sentence(&changed)["zh_translations"]
    );

    // band 可重复，但一个稳定 ID 不能代表两条译文。
    let mut invalid = writable_v3_meanings(&preserved);
    let translations = invalid["pos"][0]["senses"][0]["sentences"][0]["zh_translations"]
        .as_array_mut()
        .unwrap();
    translations.push(translations[0].clone());
    let (status, _) = save_v3_meanings_raw(
        &state,
        &bearer,
        entry_id,
        preserved["word"]["revision"].as_i64().unwrap(),
        invalid,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
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

/// 指向从未发布草稿目标的已解析成分：没有 `target_publication_id`。
fn resolved_draft_component_json(target: &Value, dialect: &str, literal: &str) -> Value {
    let mut component = resolved_component_json(target, Uuid::nil(), dialect, literal);
    component
        .as_object_mut()
        .unwrap()
        .remove("target_publication_id");
    component
}

/// 由已解析成分改成正文关联：去掉成分独有字段，补上句子区间。
fn text_link_json(component: &Value, segments: Value) -> Value {
    let mut link = component.clone();
    for field in [
        "state",
        "literal",
        "target_dialect",
        "target_form_type",
        "target_headword",
        "target_gloss",
    ] {
        link.as_object_mut().unwrap().remove(field);
    }
    link["source_segments"] = segments;
    link
}

/// 挂在 `owner` 首个词义下的例句，正文带人工关联。
fn sentence_with_text_links_json(owner: &Value, text: &str, text_links: Value) -> Value {
    json!({
        "id": Uuid::now_v7(),
        "level": "B1",
        "en_text": {
            "mode": "unified",
            "common": {
                "id": Uuid::now_v7(),
                "origin": "manual",
                "value": rich_text(text),
                "text_links": text_links
            }
        },
        "zh_text_id": Uuid::now_v7(),
        "zh_text": rich_text("测试译文。"),
        "links": [{
            "word_id": owner["word"]["id"],
            "sense_id": owner["word"]["meanings"]["pos"][0]["senses"][0]["id"],
            "role": "focus"
        }]
    })
}

/// 从未发布的草稿单词 harbour / harbor，另挂复数 harbours / harbors；词形与词义都已保存。
async fn create_unpublished_harbour_draft_with_plural(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
) -> (Value, Uuid) {
    let word_created = create_v3_with_complete_forms(state, pool, bearer).await;
    let entry_id = word_created["word"]["id"].as_str().unwrap().to_owned();
    let mut forms = word_created["word"]["forms"].clone();
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
        state,
        bearer,
        &entry_id,
        word_created["word"]["revision"].as_i64().unwrap(),
        "complete",
        forms,
    )
    .await;
    let saved = save_v3_meanings(
        state,
        bearer,
        &forms_saved,
        complete_v3_meanings_fixture(forms_saved["word"]["forms"]["pos"][0]["pos_id"].clone()),
    )
    .await;
    (saved, plural_id)
}

/// 把响应里的词义搬成可写形状，并去掉正文关联上服务端回填的只读字段（写请求禁止带）。
fn writable_v3_meanings_with_links(word: &Value) -> Value {
    let mut meanings = writable_v3_meanings(word);
    for pos in meanings["pos"].as_array_mut().into_iter().flatten() {
        for sense in pos["senses"].as_array_mut().into_iter().flatten() {
            for sentence in sense["sentences"].as_array_mut().into_iter().flatten() {
                let Some(links) = sentence["en_text"]["common"]["text_links"].as_array_mut() else {
                    continue;
                };
                for link in links {
                    let link = link.as_object_mut().unwrap();
                    link.remove("target_headword");
                    link.remove("target_gloss");
                }
            }
        }
    }
    meanings
}

async fn entry_revision(pool: &PgPool, entry_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT revision FROM lexicon.entries WHERE id = $1")
        .bind(entry_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// 来源词条**当前发布版本**里某类出引用的锚点：(目标发布版本, 范围, 目标 revision)。
async fn current_sense_ref_anchors(
    pool: &PgPool,
    source_entry_id: Uuid,
    reference_kind: &str,
) -> Vec<(Option<Uuid>, String, i64)> {
    sqlx::query_as(
        r#"
        SELECT sense_ref.target_publication_id, sense_ref.target_content_scope, sense_ref.target_revision
        FROM lexicon.entry_publication_sense_refs sense_ref
        JOIN lexicon.entries source ON source.current_publication_id = sense_ref.publication_id
        WHERE sense_ref.entry_id = $1 AND sense_ref.reference_kind = $2
        ORDER BY sense_ref.target_sense_id
        "#,
    )
    .bind(source_entry_id)
    .bind(reference_kind)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn current_publication_snapshot(pool: &PgPool, entry_id: Uuid) -> Value {
    sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE id = $1")
        .bind(current_publication_id(pool, entry_id).await)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn component_target_search_lists_never_published_drafts_by_exact_surface_when_requested(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    // 草稿由一位管理员创建，另一位来搜：草稿候选不按创建者过滤。
    let author_bearer = token(&state, seed_admin(&pool).await);
    let bearer = token(&state, seed_admin(&pool).await);
    let (draft, plural_id) =
        create_unpublished_harbour_draft_with_plural(&state, &pool, &author_bearer).await;
    let draft_entry_id = draft["word"]["id"].as_str().unwrap().to_owned();
    let (phrase_published, _) =
        create_published_v3_phrase(&state, &pool, &author_bearer, "harbour club", json!([])).await;
    let phrase_entry_id = phrase_published["word"]["id"].as_str().unwrap().to_owned();

    // 旧调用方（包含匹配、不含草稿）：只有已发布短语，每条候选照旧带 publication_id。
    let (status, found) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbour"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(
        component_match_entry_ids(&found),
        HashSet::from([phrase_entry_id.clone()]),
        "{found}"
    );
    assert!(
        found["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|candidate| candidate["publication_id"].is_string()),
        "不传 include_drafts 时响应形状不变：{found}"
    );

    // 等值匹配 + 含草稿：harbour club 不再因包含而命中；他人的草稿单词列出，且没有 publication_id。
    let (status, exact) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "Harbour", "match": "exact", "include_drafts": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exact}");
    assert_eq!(
        component_match_entry_ids(&exact),
        HashSet::from([draft_entry_id.clone()]),
        "{exact}"
    );
    assert_eq!(
        exact["total"].as_u64().unwrap(),
        exact["matches"].as_array().unwrap().len() as u64,
        "{exact}"
    );
    assert_eq!(exact["truncated"], false);
    // 夹具有两个原形、英美两侧变体，候选按 (原形 × 命中变体) 展开；每条都是草稿形状。
    for candidate in exact["matches"].as_array().unwrap() {
        assert!(
            candidate.get("publication_id").is_none(),
            "草稿候选不得带 publication_id：{candidate}"
        );
        assert_eq!(candidate["kind"], "word");
        assert_eq!(candidate["matches"], json!([]));
        assert_eq!(candidate["matched_form_type"], "base");
        let senses = candidate["senses"].as_array().unwrap();
        assert!(
            !senses.is_empty(),
            "草稿候选也要带词义供级联第三层：{candidate}"
        );
        assert!(
            senses
                .iter()
                .all(|sense| sense.get("publication_id").is_none()),
            "{candidate}"
        );
    }

    // 等值匹配、不含草稿：没有任何已发布词面等于 harbour。
    let (status, none) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbour", "match": "exact"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{none}");
    assert_eq!(none["matches"], json!([]));
    assert_eq!(none["total"], 0);

    // 屈折词形按同一套归一化等值命中原形词条，命中词形是复数。
    let (status, plural) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbours", "match": "exact", "include_drafts": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plural}");
    assert_eq!(
        component_match_entry_ids(&plural),
        HashSet::from([draft_entry_id.clone()])
    );
    assert!(
        plural["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|candidate| {
                candidate["matched_form_id"] == json!(plural_id)
                    && candidate["matched_form_type"] == "plural"
            }),
        "{plural}"
    );

    // 包含匹配 + 含草稿：两者都在；档位先于状态——等于关键字的草稿排在前缀命中的已发布短语前。
    let (status, both) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbour", "include_drafts": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{both}");
    let order = both["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["entry_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(order.first(), Some(&draft_entry_id), "{both}");
    assert!(order.contains(&phrase_entry_id), "{both}");

    // 游标绑定匹配方式与是否含草稿：换任一开关即失效。
    let (status, first) = search_component_targets(
        &state,
        &bearer,
        json!({"schema_version": 3, "q": "harbour", "include_drafts": true, "page_size": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let cursor = first["next_cursor"]
        .as_str()
        .expect("多条命中、每页一条应有下一页")
        .to_owned();
    for body in [
        json!({"schema_version": 3, "q": "harbour", "match": "exact", "include_drafts": true, "page_size": 1, "cursor": cursor}),
        json!({"schema_version": 3, "q": "harbour", "page_size": 1, "cursor": cursor}),
    ] {
        let (status, rejected) = search_component_targets(&state, &bearer, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
        assert_eq!(rejected["field"], "cursor", "{rejected}");
    }
}

#[sqlx::test]
async fn text_links_may_target_never_published_drafts_and_upgrade_after_the_target_publishes(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let author_bearer = token(&state, seed_admin(&pool).await);
    let bearer = token(&state, seed_admin(&pool).await);
    // 目标：另一位管理员的草稿单词 harbour，从未发布。
    let target = create_ready_v3_draft_with_sentences(
        &state,
        &pool,
        &author_bearer,
        &["The harbour is calm."],
    )
    .await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    // 宿主：短语草稿，例句里把 harbour 关联到那份草稿。
    let host =
        create_v3_phrase_with_sense_components(&state, &bearer, "harbour side", json!([])).await;
    let host_entry_id = Uuid::parse_str(host["word"]["id"].as_str().unwrap()).unwrap();
    let link = text_link_json(
        &resolved_draft_component_json(&target, "uk", "harbour"),
        json!([{"start": 2, "end": 9, "surface": "harbour"}]),
    );
    let mut meanings = writable_v3_meanings(&host);
    meanings["pos"][0]["senses"][0]["sentences"] = json!([sentence_with_text_links_json(
        &host,
        "A harbour sentence.",
        json!([link])
    )]);
    let saved = save_v3_meanings(&state, &bearer, &host, meanings).await;
    let sentence = &saved["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0];
    let saved_link = &sentence["en_text"]["common"]["text_links"][0];
    assert!(
        saved_link.get("target_publication_id").is_none(),
        "草稿目标没有发布版本可填：{saved_link}"
    );
    assert_eq!(
        saved_link["target_headword"],
        target["word"]["presentation"]["label"]
    );
    assert_eq!(
        saved_link["target_gloss"],
        target["word"]["meanings"]["pos"][0]["senses"][0]["definitions"][0]["content"]["text"]
    );
    let association = sentence["associations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|association| association["origin"] == "manual")
        .expect("人工关联应投影到 associations");
    assert_eq!(association["target_word_id"], target["word"]["id"]);
    assert!(
        association.get("target_publication_id").is_none(),
        "{association}"
    );

    // 宿主照常发布：引用记 draft 范围、锚在目标当时的 entry revision，快照里的关联没有发布版本。
    let (status, published) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    let target_revision = entry_revision(&pool, target_entry_id).await;
    assert_eq!(
        current_sense_ref_anchors(&pool, host_entry_id, "text_link").await,
        vec![(None, "draft".to_owned(), target_revision)]
    );
    let snapshot = current_publication_snapshot(&pool, host_entry_id).await;
    let snapshot_link = &snapshot["meanings"]["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]
        ["text_links"][0];
    assert_eq!(snapshot_link["target_word_id"], target["word"]["id"]);
    assert!(
        snapshot_link.get("target_publication_id").is_none(),
        "{snapshot_link}"
    );

    // 目标发布后，宿主下一次保存由服务端补上目标当前发布版本，再发布即为 publication 范围。
    let (status, target_published) = publish_ready_v3(&state, &author_bearer, &target).await;
    assert_eq!(status, StatusCode::CREATED, "{target_published}");
    let target_publication = current_publication_id(&pool, target_entry_id).await;
    let resaved = save_v3_meanings(
        &state,
        &bearer,
        &published,
        writable_v3_meanings_with_links(&published),
    )
    .await;
    let upgraded = &resaved["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]
        ["text_links"][0];
    assert_eq!(
        upgraded["target_publication_id"],
        json!(target_publication),
        "{upgraded}"
    );
    let (status, republished) = publish_ready_v3(&state, &bearer, &resaved).await;
    assert_eq!(status, StatusCode::CREATED, "{republished}");
    assert_eq!(
        current_sense_ref_anchors(&pool, host_entry_id, "text_link").await,
        vec![(
            Some(target_publication),
            "publication".to_owned(),
            target_published["word"]["revision"].as_i64().unwrap()
        )]
    );
}

#[sqlx::test]
async fn text_links_to_draft_targets_reject_saving_after_the_target_sense_disappears(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let author_bearer = token(&state, seed_admin(&pool).await);
    let bearer = token(&state, seed_admin(&pool).await);
    let target = create_ready_v3_draft_with_sentences(
        &state,
        &pool,
        &author_bearer,
        &["The harbour is calm."],
    )
    .await;
    let host =
        create_v3_phrase_with_sense_components(&state, &bearer, "harbour side", json!([])).await;
    let link = text_link_json(
        &resolved_draft_component_json(&target, "uk", "harbour"),
        json!([{"start": 2, "end": 9, "surface": "harbour"}]),
    );
    let mut meanings = writable_v3_meanings(&host);
    meanings["pos"][0]["senses"][0]["sentences"] = json!([sentence_with_text_links_json(
        &host,
        "A harbour sentence.",
        json!([link])
    )]);
    let saved = save_v3_meanings(&state, &bearer, &host, meanings).await;

    // 目标作者把整套词义换掉：被关联的词义节点从草稿里移除。
    let replacement =
        complete_v3_meanings_fixture(target["word"]["forms"]["pos"][0]["pos_id"].clone());
    save_v3_meanings(&state, &author_bearer, &target, replacement).await;

    // 宿主再保存：草稿目标里已没有这个词义，422 且 issue 锚在 text_links。
    let (status, problem) = save_v3_meanings_raw(
        &state,
        &bearer,
        saved["word"]["id"].as_str().unwrap(),
        saved["word"]["revision"].as_i64().unwrap(),
        writable_v3_meanings_with_links(&saved),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(
        problem["field_issues"]
            .as_array()
            .is_some_and(|issues| issues.iter().any(|issue| issue["field"] == "text_links")),
        "{problem}"
    );
}

#[sqlx::test]
async fn phrase_components_may_target_never_published_drafts_and_upgrade_after_the_target_publishes(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let author_bearer = token(&state, seed_admin(&pool).await);
    let bearer = token(&state, seed_admin(&pool).await);
    let target = create_ready_v3_draft_with_sentences(
        &state,
        &pool,
        &author_bearer,
        &["The harbour is calm."],
    )
    .await;
    let target_entry_id = Uuid::parse_str(target["word"]["id"].as_str().unwrap()).unwrap();
    let component = resolved_draft_component_json(&target, "uk", "harbour");
    let saved =
        create_v3_phrase_with_sense_components(&state, &bearer, "harbour side", json!([component]))
            .await;
    let host_entry_id = Uuid::parse_str(saved["word"]["id"].as_str().unwrap()).unwrap();
    let usage = &saved["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"][0];
    assert_eq!(usage["state"], "resolved", "{usage}");
    assert_eq!(usage["target_word_id"], target["word"]["id"]);
    assert!(
        usage.get("target_publication_id").is_none(),
        "草稿目标没有发布版本可填：{usage}"
    );

    // 目标作者改了释义文案：草稿目标是活的，宿主重存不被「文案不一致」拒掉，成分文案由服务端刷新。
    let mut retitled = writable_v3_meanings(&target);
    retitled["pos"][0]["senses"][0]["definitions"][0]["content"] = rich_text("码头");
    let target = save_v3_meanings(&state, &author_bearer, &target, retitled).await;
    let refreshed = save_v3_meanings(&state, &bearer, &saved, writable_v3_meanings(&saved)).await;
    let refreshed_usage =
        &refreshed["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"][0];
    assert_eq!(refreshed_usage["target_gloss"], "码头", "{refreshed_usage}");
    assert!(
        refreshed_usage.get("target_publication_id").is_none(),
        "{refreshed_usage}"
    );

    let (status, published) = publish_ready_v3(&state, &bearer, &refreshed).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    let target_revision = entry_revision(&pool, target_entry_id).await;
    assert_eq!(
        current_sense_ref_anchors(&pool, host_entry_id, "phrase_component").await,
        vec![(None, "draft".to_owned(), target_revision)]
    );

    let (status, target_published) = publish_ready_v3(&state, &author_bearer, &target).await;
    assert_eq!(status, StatusCode::CREATED, "{target_published}");
    let target_publication = current_publication_id(&pool, target_entry_id).await;
    let resaved = save_v3_meanings(
        &state,
        &bearer,
        &published,
        writable_v3_meanings_with_links(&published),
    )
    .await;
    assert_eq!(
        resaved["word"]["meanings"]["pos"][0]["senses"][0]["component_usages"][0]["target_publication_id"],
        json!(target_publication),
        "{resaved}"
    );
    let (status, republished) = publish_ready_v3(&state, &bearer, &resaved).await;
    assert_eq!(status, StatusCode::CREATED, "{republished}");
    assert_eq!(
        current_sense_ref_anchors(&pool, host_entry_id, "phrase_component").await,
        vec![(
            Some(target_publication),
            "publication".to_owned(),
            target_published["word"]["revision"].as_i64().unwrap()
        )]
    );
}

#[sqlx::test]
async fn component_target_search_recovers_after_missing_generation_is_repaired(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let (published, _) =
        create_published_v3_phrase(&state, &pool, &bearer, "time being", json!([])).await;
    let entry_id = published["word"]["id"].as_str().unwrap().to_owned();

    sqlx::query("DELETE FROM lexicon.sentence_discovery_generation")
        .execute(&pool)
        .await
        .unwrap();
    let body = json!({"schema_version": 3, "q": "time", "page_size": 50});
    let (status, failed) = search_component_targets(&state, &bearer, body.clone()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{failed}");
    assert_eq!(failed["code"], "internal_error");

    const REPAIR: &str = include_str!(
        "../migrations/20260906170000_repair_missing_sentence_discovery_generation.up.sql"
    );
    sqlx::raw_sql(REPAIR).execute(&pool).await.unwrap();
    let generation_query = "SELECT generation, last_txid FROM lexicon.sentence_discovery_generation WHERE singleton = TRUE";
    let repaired: (i64, Option<i64>) = sqlx::query_as(generation_query)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(repaired.0 > 1);
    assert_eq!(repaired.1, None);

    let (status, found) = search_component_targets(&state, &bearer, body).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert!(component_match_entry_ids(&found).contains(&entry_id));

    // The existing statement trigger must still bump once per transaction.
    let mut tx = pool.begin().await.unwrap();
    for _ in 0..2 {
        sqlx::query("UPDATE lexicon.surface_sources SET event_offset = event_offset WHERE FALSE")
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    let bumped: (i64, Option<i64>) = sqlx::query_as(generation_query)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(bumped.0, repaired.0 + 1);
    assert!(bumped.1.is_some());

    sqlx::raw_sql(REPAIR).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20260906170000_repair_missing_sentence_discovery_generation.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let retained: (i64, Option<i64>) = sqlx::query_as(generation_query)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        retained, bumped,
        "reapply/rollback must preserve healthy state"
    );
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

#[sqlx::test]
async fn v3_detection_drops_suggested_pos_missing_from_catalog(pool: PgPool) {
    // 2026-09-06 起目录只种五个基础词性；内置词典映射出的介词等编码不在目录里时，
    // V3 检测必须像 V2 一样只建议目录现存的词性，否则前端会拿到无法保存的 pos。
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    seed_dictionary_word(&pool, "before").await;
    sqlx::query(
        "UPDATE dictionary.terms SET pos = ARRAY['preposition', 'verb', 'noun'] WHERE normalized_term = 'before'",
    )
    .execute(&pool)
    .await
    .expect("应能把内置词典词性改成含已下线编码");

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
            "surface": "before"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    // 介词被过滤掉；其余保持词典给出的顺序（verb 在 noun 前），不按目录排序重排。
    assert_eq!(
        detection["suggested_pos"],
        json!(["verb", "noun"]),
        "目录里不存在的介词不得进入建议，且保持词典顺序"
    );
    assert_eq!(
        detection["builtin_dictionary"]["suggested_pos"],
        json!(["verb", "noun"])
    );
}

#[sqlx::test]
async fn v3_derivative_multiple_senses_publish_and_remove_independently(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let target = create_v3_with_annotated_complete_forms(&state, &pool, &bearer).await;
    let mut target_content =
        complete_v3_meanings_fixture(target["word"]["forms"]["pos"][0]["pos_id"].clone());
    let mut second = target_content["pos"][0]["senses"][0].clone();
    second["id"] = json!(Uuid::now_v7());
    second["definitions"][0]["id"] = json!(Uuid::now_v7());
    second["definitions"][0]["content_id"] = json!(Uuid::now_v7());
    second["definitions"][0]["content"] = rich_text("第二个派生词义");
    target_content["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let target = save_v3_meanings(&state, &bearer, &target, target_content).await;
    let (status, target) = publish_ready_v3(&state, &bearer, &target).await;
    assert_eq!(status, StatusCode::CREATED, "{target}");
    let source = create_v3_with_annotated_complete_forms(&state, &pool, &bearer).await;
    let mut source_content =
        complete_v3_meanings_fixture(source["word"]["forms"]["pos"][0]["pos_id"].clone());
    let relations: Vec<Value> = target["word"]["meanings"]["pos"][0]["senses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|sense| {
            json!({
                "id": Uuid::now_v7(), "relation": "derivative", "score": "80.00",
                "target_word_id": target["word"]["id"], "target_sense_id": sense["id"]
            })
        })
        .collect();
    assert_eq!(relations.len(), 2);
    source_content["pos"][0]["senses"][0]["relations"] = json!(relations);
    let saved = save_v3_meanings(&state, &bearer, &source, source_content).await;
    let (status, published) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    let entry_id = Uuid::parse_str(source["word"]["id"].as_str().unwrap()).unwrap();
    let (status, reloaded) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let stored = &reloaded["word"]["meanings"]["pos"][0]["senses"][0]["relations"];
    assert_eq!(stored.as_array().unwrap().len(), 2);
    for (index, expected) in relations.iter().enumerate() {
        assert_eq!(stored[index]["id"], expected["id"]);
        assert_eq!(
            stored[index]["target_sense_id"],
            expected["target_sense_id"]
        );
    }
    assert_ne!(stored[0]["target_gloss"], stored[1]["target_gloss"]);
    let refs: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publication_sense_refs WHERE publication_id = (SELECT current_publication_id FROM lexicon.entries WHERE id = $1) AND source_node_id = ANY($2)")
        .bind(entry_id).bind(relations.iter().map(|item| Uuid::parse_str(item["id"].as_str().unwrap()).unwrap()).collect::<Vec<_>>())
        .fetch_one(&pool).await.unwrap();
    assert_eq!(refs, 2);
    let mut content = writable_v3_meanings(&reloaded);
    content["pos"][0]["senses"][0]["relations"] = json!([relations[1]]);
    let saved = save_v3_meanings(&state, &bearer, &reloaded, content).await;
    let (status, republished) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{republished}");
    let remaining = &republished["word"]["meanings"]["pos"][0]["senses"][0]["relations"];
    assert_eq!(remaining.as_array().unwrap().len(), 1);
    assert_eq!(remaining[0]["id"], relations[1]["id"]);
    assert_eq!(
        remaining[0]["target_sense_id"],
        relations[1]["target_sense_id"]
    );
    let refs: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publication_sense_refs WHERE publication_id = (SELECT current_publication_id FROM lexicon.entries WHERE id = $1) AND source_node_id = ANY($2)")
        .bind(entry_id).bind(relations.iter().map(|item| Uuid::parse_str(item["id"].as_str().unwrap()).unwrap()).collect::<Vec<_>>())
        .fetch_one(&pool).await.unwrap();
    assert_eq!(refs, 1);
}

/// 按 code 取形状问题的 field，不靠 field_issues 的位置——夹具以后多出别的问题时，
/// 位置依赖会以「field 不相等」误报，而不是指出夹具变了。
fn shape_issue_field(body: &Value) -> Option<&str> {
    body["field_issues"].as_array()?.iter().find_map(|issue| {
        (issue["code"] == "relation_target_shape_invalid").then(|| issue["field"].as_str())?
    })
}

/// 半绑定关系必须在保存时就被拒，不能留到前端重开词条时炸开。
///
/// `bound_target()` 是 zip，给了词条没给词义时它返回 None，这种形状因此曾被当成
/// 「未绑定」放行；而前端解析这个形状会直接抛错，表现是编辑器白屏而非一条校验提示。
#[sqlx::test]
async fn v3_relations_reject_half_bound_target_shapes(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let target = create_v3_with_annotated_complete_forms(&state, &pool, &bearer).await;
    let source = create_v3_with_annotated_complete_forms(&state, &pool, &bearer).await;
    let source_id = source["word"]["id"].as_str().unwrap();
    let target_id = target["word"]["id"].as_str().unwrap().to_owned();
    let base_revision = source["word"]["revision"].as_i64().unwrap();
    let content = complete_v3_meanings_fixture(source["word"]["forms"]["pos"][0]["pos_id"].clone());

    // 有词条没词义。
    let mut only_word = content.clone();
    only_word["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(), "relation": "derivative", "score": "80.00",
        "target_word_id": target_id
    }]);
    let only_word_for_save = only_word.clone();
    let (status, rejected) =
        save_v3_meanings_raw(&state, &bearer, source_id, base_revision, only_word).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert!(
        has_issue(&rejected, "relation_target_shape_invalid"),
        "给了词条没给词义应被拒：{rejected}"
    );
    assert_eq!(
        shape_issue_field(&rejected),
        Some("target_sense_id"),
        "缺的是词义，field 要指向它：{rejected}"
    );

    // 点「保存草稿」走的是 save 意图，而 semantic_issues 只在 complete 时回出，
    // 这条路径此前一路写到库层、撞 CHECK、兜底成 500。它才是用户实际会走的那条。
    let (status, rejected_on_save) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": base_revision,
            "intent": "save",
            "content": only_word_for_save
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "保存草稿也要拒掉半绑定，不能兜底成 500：{rejected_on_save}"
    );
    assert!(
        has_issue(&rejected_on_save, "relation_target_shape_invalid"),
        "{rejected_on_save}"
    );

    // 有词义没词条。
    let mut only_sense = content;
    only_sense["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(), "relation": "derivative", "score": "80.00",
        "target_sense_id": Uuid::now_v7()
    }]);
    let (status, rejected) =
        save_v3_meanings_raw(&state, &bearer, source_id, base_revision, only_sense).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert!(
        has_issue(&rejected, "relation_target_shape_invalid"),
        "给了词义没给词条应被拒：{rejected}"
    );
    assert_eq!(
        shape_issue_field(&rejected),
        Some("target_word_id"),
        "缺的是词条，field 要指向它：{rejected}"
    );
}

#[sqlx::test]
async fn v3_relations_require_explicit_sense_binding_and_keep_same_name_text(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let target = create_v3_with_annotated_complete_forms(&state, &pool, &bearer).await;
    let source = create_v3_with_annotated_complete_forms(&state, &pool, &bearer).await;
    let source_id = source["word"]["id"].as_str().unwrap();
    let target_id = target["word"]["id"].as_str().unwrap();
    let relation_id = Uuid::now_v7();
    let mut content =
        complete_v3_meanings_fixture(source["word"]["forms"]["pos"][0]["pos_id"].clone());
    content["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id, "relation": "synonym", "score": "80.00",
        "prebound_target_word_id": target_id
    }]);
    let (status, rejected) = save_v3_meanings_raw(
        &state,
        &bearer,
        source_id,
        source["word"]["revision"].as_i64().unwrap(),
        content.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    content["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id, "relation": "synonym", "score": "80.00",
        "pending_target_headword": "harbour"
    }]);
    let source = save_v3_meanings(&state, &bearer, &source, content.clone()).await;
    let source_revision = source["word"]["revision"].clone();
    let mut target_content =
        complete_v3_meanings_fixture(target["word"]["forms"]["pos"][0]["pos_id"].clone());
    let target_sense_id = target_content["pos"][0]["senses"][0]["id"].clone();
    let mut unreferenced_sense = target_content["pos"][0]["senses"][0].clone();
    unreferenced_sense["id"] = json!(Uuid::now_v7());
    unreferenced_sense["definitions"][0]["id"] = json!(Uuid::now_v7());
    unreferenced_sense["definitions"][0]["content_id"] = json!(Uuid::now_v7());
    target_content["pos"][0]["senses"]
        .as_array_mut()
        .unwrap()
        .insert(0, unreferenced_sense);
    let target = save_v3_meanings(&state, &bearer, &target, target_content).await;
    let (status, unchanged) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{source_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unchanged["word"]["revision"], source_revision);
    let source = save_v3_meanings(&state, &bearer, &source, content.clone()).await;
    assert!(
        source["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_word_id"]
            .is_null()
    );
    let (status, search) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=harbour&kind=word&include_drafts=true"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{search}");
    content["pos"][0]["senses"][0]["relations"] = json!([{
        "id": relation_id, "relation": "synonym", "score": "80.00",
        "target_word_id": target_id, "target_sense_id": target_sense_id
    }]);
    let source = save_v3_meanings(&state, &bearer, &source, content).await;
    assert_eq!(
        source["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0]["target_word_id"],
        target_id
    );
    let mut removed = writable_v3_meanings(&target);
    removed["pos"][0]["senses"] = json!([]);
    let (status, rejected) = call(&state, Method::PUT, &format!("{ROOT}/entries/{target_id}/steps/meanings"),
        &bearer, None, Some(json!({"schema_version": 3, "base_revision": target["word"]["revision"], "intent": "save", "content": removed}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert!(
        rejected["field_issues"]
            .as_array()
            .unwrap_or_else(|| panic!("unexpected error: {rejected}"))
            .iter()
            .any(|issue| issue["code"] == "relation_target_unavailable"
                && issue["node_id"] == target_sense_id)
    );
}

// Annotation tests use unique SQLx test names so they cannot reuse another task's databases.
async fn entry_annotations_create_body(
    state: &AppState,
    bearer: &str,
    surface: &str,
    headwords: Value,
) -> Value {
    let (status, detection) = call(
        state,
        Method::POST,
        &format!("{ROOT}/detections"),
        bearer,
        None,
        Some(json!({"schema_version":3,"language":"en","kind":"word","surface":surface})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    json!({"schema_version":3,"detection_id":detection["detection_id"],"kind":"word","headwords":headwords})
}

async fn entry_annotations_submit(
    state: &AppState,
    bearer: &str,
    key: Uuid,
    body: &mut Value,
) -> (StatusCode, Value) {
    for _ in 0..3 {
        let (status, response) = call(
            state,
            Method::POST,
            &format!("{ROOT}/entries"),
            bearer,
            Some(key),
            Some(body.clone()),
        )
        .await;
        if matches!(
            response["code"].as_str(),
            Some("surface_match_acknowledgement_required" | "surface_matches_changed")
        ) {
            let token = &response["meta"]["surface_match_page"]["surface_confirmation_token"];
            assert!(token.is_string(), "{response}");
            body["confirmed_surface_match_token"] = token.clone();
        } else {
            return (status, response);
        }
    }
    panic!("surface confirmation did not stabilize");
}

fn entry_annotations_updates(conflict: &Value, labels: &[&str]) -> Value {
    let entries = conflict["meta"]["annotation_conflict"]["entries"]
        .as_array()
        .unwrap();
    assert_eq!(entries.len(), labels.len(), "{conflict}");
    json!(entries.iter().zip(labels).map(|(entry, label)| json!({
        "entry_id":entry["entry_id"],"annotation":label,"base_annotation_revision":entry["annotation_revision"]
    })).collect::<Vec<_>>())
}

#[sqlx::test]
async fn entry_annotations_atomic_third_entry_edit_and_idempotency(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    let first = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    assert_eq!(first["word"]["annotation"], Value::Null);
    assert_eq!(first["word"]["annotation_revision"], 1);
    let first_id = first["word"]["id"].as_str().unwrap();
    let mut body = entry_annotations_create_body(
        &state,
        &bearer,
        "harbour",
        json!({"mode":"unified","common":"harbour"}),
    )
    .await;
    let key = Uuid::now_v7();
    let (status, required) = entry_annotations_submit(&state, &bearer, key, &mut body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{required}");
    assert_eq!(required["code"], "annotation_conflict");
    assert_eq!(
        required["meta"]["annotation_conflict"]["reason"],
        "required"
    );
    // 自己创建的条目也要带 created_by，且指向自己——前端据此把这一行开放编辑。
    for entry in required["meta"]["annotation_conflict"]["entries"]
        .as_array()
        .unwrap()
    {
        assert_eq!(entry["created_by"], json!(admin_id), "{required}");
    }
    body["annotation"] = json!(" Two ");
    body["annotation_updates"] = entry_annotations_updates(&required, &[" One "]);
    let path = format!("{ROOT}/entries");
    let (a, b) = tokio::join!(
        call(
            &state,
            Method::POST,
            &path,
            &bearer,
            Some(key),
            Some(body.clone())
        ),
        call(
            &state,
            Method::POST,
            &path,
            &bearer,
            Some(key),
            Some(body.clone())
        )
    );
    assert_eq!(a.0, StatusCode::CREATED, "{a:?}");
    assert_eq!(a, b, "concurrent identical commands must return one entry");
    let (_, second) = a;
    assert_eq!(second["word"]["annotation"], "Two");
    let (status, replay) = entry_annotations_submit(&state, &bearer, key, &mut body).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay, second);
    let second_id = second["word"]["id"].as_str().unwrap();
    let (_, read_first) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{first_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(read_first["word"]["annotation"], "One");
    assert_eq!(read_first["word"]["annotation_revision"], 2);
    assert_eq!(read_first["word"]["revision"], first["word"]["revision"]);
    assert_eq!(
        read_first["word"]["has_unpublished_changes"],
        first["word"]["has_unpublished_changes"]
    );
    let mut changed_body = body.clone();
    changed_body["annotation"] = json!("changed");
    let (_, response) = entry_annotations_submit(&state, &bearer, key, &mut changed_body).await;
    assert_eq!(response["code"], "idempotency_conflict");

    let mut third_body = entry_annotations_create_body(
        &state,
        &bearer,
        "harbour",
        json!({"mode":"unified","common":"harbour"}),
    )
    .await;
    let third_key = Uuid::now_v7();
    let (_, third_required) =
        entry_annotations_submit(&state, &bearer, third_key, &mut third_body).await;
    third_body["annotation"] = json!("THREE");
    third_body["annotation_updates"] =
        entry_annotations_updates(&third_required, &["three", "updated"]);
    let (_, duplicate) =
        entry_annotations_submit(&state, &bearer, third_key, &mut third_body).await;
    assert_eq!(
        duplicate["meta"]["annotation_conflict"]["reason"],
        "duplicate"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
    let labels: Vec<Option<String>> =
        sqlx::query_scalar("SELECT annotation FROM lexicon.entries ORDER BY annotation")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(labels, vec![Some("One".to_owned()), Some("Two".to_owned())]);
    third_body["annotation_updates"] =
        entry_annotations_updates(&third_required, &["First", "Second"]);
    let mut stale = third_body.clone();
    stale["annotation_updates"][0]["base_annotation_revision"] = json!(999);
    let (_, response) = entry_annotations_submit(&state, &bearer, third_key, &mut stale).await;
    assert_eq!(
        response["meta"]["annotation_conflict"]["reason"],
        "revision_conflict"
    );
    // A late INSERT error occurs after old annotations have been updated; all writes must roll back.
    sqlx::query("ALTER TABLE lexicon.entries ADD CONSTRAINT entry_annotations_injected_failure CHECK (annotation IS DISTINCT FROM 'FAIL')").execute(&pool).await.unwrap();
    let mut failing = third_body.clone();
    failing["annotation"] = json!("FAIL");
    let (status, _) = entry_annotations_submit(&state, &bearer, third_key, &mut failing).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let labels: Vec<Option<String>> =
        sqlx::query_scalar("SELECT annotation FROM lexicon.entries ORDER BY annotation")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(labels, vec![Some("One".to_owned()), Some("Two".to_owned())]);
    sqlx::query("ALTER TABLE lexicon.entries DROP CONSTRAINT entry_annotations_injected_failure")
        .execute(&pool)
        .await
        .unwrap();
    let (status, third) =
        entry_annotations_submit(&state, &bearer, third_key, &mut third_body).await;
    assert_eq!(status, StatusCode::CREATED, "{third}");

    let edit_path = format!("{ROOT}/entries/{second_id}/annotation");
    let (_, second_now) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{second_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    let revision = second_now["word"]["annotation_revision"].clone();
    let (_, conflict) = call(
        &state,
        Method::PATCH,
        &edit_path,
        &bearer,
        None,
        Some(json!({"annotation":"three","base_annotation_revision":revision})),
    )
    .await;
    assert_eq!(
        conflict["meta"]["annotation_conflict"]["reason"],
        "duplicate"
    );
    let (status, saved) = call(
        &state,
        Method::PATCH,
        &edit_path,
        &bearer,
        None,
        Some(json!({"annotation":"😀".repeat(20),"base_annotation_revision":revision})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, stale) = call(
        &state,
        Method::PATCH,
        &edit_path,
        &bearer,
        None,
        Some(json!({"annotation":"new","base_annotation_revision":revision})),
    )
    .await;
    assert_eq!(
        stale["meta"]["annotation_conflict"]["reason"],
        "revision_conflict"
    );
    let (status, invalid) = call(&state, Method::PATCH, &edit_path, &bearer, None, Some(json!({"annotation":"😀".repeat(21),"base_annotation_revision":saved["annotation_revision"]}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
    let (_, listed) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries?page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert!(
        listed["words"]
            .as_array()
            .unwrap()
            .iter()
            .all(|word| word["annotation"].is_string() && word["annotation_revision"].is_number())
    );
}

// Used only by legacy scenarios that explicitly opt into homonym fixtures.
async fn create_annotated_fixture(
    state: &AppState,
    bearer: &str,
    key: Uuid,
    mut body: Value,
) -> (StatusCode, Value) {
    let (status, response) = call(
        state,
        Method::POST,
        &format!("{ROOT}/entries"),
        bearer,
        Some(key),
        Some(body.clone()),
    )
    .await;
    if response["code"] != "annotation_conflict" {
        return (status, response);
    }
    assert_eq!(
        response["meta"]["annotation_conflict"]["reason"], "required",
        "{response}"
    );
    body["annotation"] = json!(format!("new-{}", &key.simple().to_string()[20..]));
    body["annotation_updates"] = json!(response["meta"]["annotation_conflict"]["entries"].as_array().unwrap().iter().map(|entry| {
        let label = entry["annotation"].as_str().map(str::to_owned).unwrap_or_else(|| format!("old-{}", &entry["entry_id"].as_str().unwrap()[24..]));
        json!({"entry_id":entry["entry_id"],"annotation":label,"base_annotation_revision":entry["annotation_revision"]})
    }).collect::<Vec<_>>());
    call(
        state,
        Method::POST,
        &format!("{ROOT}/entries"),
        bearer,
        Some(key),
        Some(body),
    )
    .await
}

#[sqlx::test]
async fn entry_annotations_variant_only_and_distinct_create_edit_race(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let first = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let first_id = first["word"]["id"].as_str().unwrap();
    let mut forms = first["word"]["forms"].clone();
    forms["pos"][0]["forms"][1]["form_type"] = json!("plural");
    forms["pos"][0]["forms"][1]["regional_variants"]["uk"]["spelling"] = json!("harbours");
    forms["pos"][0]["forms"][1]["regional_variants"]["us"]["spelling"] = json!("harbors");
    save_v3_forms_after_impact(
        &state,
        &bearer,
        first_id,
        first["word"]["revision"].as_i64().unwrap(),
        "complete",
        forms,
    )
    .await;
    let mut variant = entry_annotations_create_body(
        &state,
        &bearer,
        "harbours",
        json!({"mode":"unified","common":"harbours"}),
    )
    .await;
    let (status, created) =
        entry_annotations_submit(&state, &bearer, Uuid::now_v7(), &mut variant).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "ordinary form warning must not require annotation: {created}"
    );
    assert_eq!(created["word"]["annotation"], Value::Null);
    let mut a = entry_annotations_create_body(
        &state,
        &bearer,
        "harbour",
        json!({"mode":"unified","common":"harbour"}),
    )
    .await;
    let mut b = entry_annotations_create_body(
        &state,
        &bearer,
        "harbour",
        json!({"mode":"unified","common":"harbour"}),
    )
    .await;
    let ka = Uuid::now_v7();
    let kb = Uuid::now_v7();
    let (_, ra) = entry_annotations_submit(&state, &bearer, ka, &mut a).await;
    let (_, rb) = entry_annotations_submit(&state, &bearer, kb, &mut b).await;
    a["annotation"] = json!("new-a");
    a["annotation_updates"] = entry_annotations_updates(&ra, &["old"]);
    b["annotation"] = json!("new-b");
    b["annotation_updates"] = entry_annotations_updates(&rb, &["old"]);
    let path = format!("{ROOT}/entries");
    let edit_path = format!("{ROOT}/entries/{first_id}/annotation");
    let (a, b, edit) = tokio::join!(
        call(&state, Method::POST, &path, &bearer, Some(ka), Some(a)),
        call(&state, Method::POST, &path, &bearer, Some(kb), Some(b)),
        call(
            &state,
            Method::PATCH,
            &edit_path,
            &bearer,
            None,
            Some(json!({"annotation":"edited","base_annotation_revision":1}))
        )
    );
    let results = [&a, &b, &edit];
    assert_eq!(
        results.iter().filter(|r| r.0.is_success()).count(),
        1,
        "{results:?}"
    );
    assert!(
        results
            .iter()
            .all(|r| r.0.is_success() || r.0 == StatusCode::CONFLICT),
        "{results:?}"
    );
    let (revision, count): (i64,i64) = sqlx::query_as("SELECT annotation_revision, (SELECT count(*) FROM lexicon.entries) FROM lexicon.entries WHERE id = $1")
        .bind(Uuid::parse_str(first_id).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(revision, 2);
    assert_eq!(
        count,
        2 + i64::from(a.0.is_success()) + i64::from(b.0.is_success())
    );
}

#[sqlx::test]
async fn surface_confirm_bugfix_first_explicit_confirmation_http(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    create_v3_with_complete_forms(&state, &pool, &bearer).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, tsz_rust::router(state))
            .await
            .unwrap()
    });
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{address}{ROOT}/detections"))
        .bearer_auth(&bearer)
        .header("content-type", "application/json")
        .body(
            json!({"schema_version":3,"language":"en","kind":"word","surface":"harbour"})
                .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let detection: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    let mut body = json!({"schema_version":3,"detection_id":detection["detection_id"],"kind":"word",
        "headwords":{"mode":"unified","common":"harbour"},
        "confirmed_surface_match_token":detection["surface_match_page"]["surface_confirmation_token"]});
    assert!(
        body["confirmed_surface_match_token"].is_string(),
        "{detection}"
    );
    let key = Uuid::now_v7();
    let response = client
        .post(format!("http://{address}{ROOT}/entries"))
        .bearer_auth(&bearer)
        .header("content-type", "application/json")
        .header("Idempotency-Key", key.to_string())
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 409);
    let conflict: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(
        conflict["code"], "annotation_conflict",
        "first explicit confirmation: {conflict}"
    );
    body["annotation"] = json!("new harbour");
    body["annotation_updates"] = entry_annotations_updates(&conflict, &["old harbour"]);
    let response = client
        .post(format!("http://{address}{ROOT}/entries"))
        .bearer_auth(&bearer)
        .header("content-type", "application/json")
        .header("Idempotency-Key", key.to_string())
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let created = response.text().await.unwrap();
    assert_eq!(status, 201, "single annotation save: {created}");
    server.abort();
}

#[sqlx::test]
async fn surface_confirm_bugfix_rejects_changed_evidence(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let first = create_v3_with_complete_forms(&state, &pool, &bearer).await;
    for scenario in ["headwords", "annotation", "new_match", "policy"] {
        let (_, detection) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/detections"),
            &bearer,
            None,
            Some(json!({"schema_version":3,"language":"en","kind":"word","surface":"harbour"})),
        )
        .await;
        let mut body = json!({"schema_version":3,"detection_id":detection["detection_id"],"kind":"word",
            "headwords":{"mode":"unified","common":"harbour"},
            "confirmed_surface_match_token":detection["surface_match_page"]["surface_confirmation_token"]});
        match scenario {
            "headwords" => {
                body["headwords"] =
                    json!({"mode":"distinguish","uk":"harbour","us":"harbor","source_dialect":"uk"})
            }
            "annotation" => {
                let (status, result) = call(
                    &state,
                    Method::PATCH,
                    &format!(
                        "{ROOT}/entries/{}/annotation",
                        first["word"]["id"].as_str().unwrap()
                    ),
                    &bearer,
                    None,
                    Some(json!({"annotation":"changed","base_annotation_revision":1})),
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{result}");
            }
            "new_match" => {
                let mut fresh = entry_annotations_create_body(
                    &state,
                    &bearer,
                    "harbour",
                    json!({"mode":"unified","common":"harbour"}),
                )
                .await;
                let key = Uuid::now_v7();
                let (_, conflict) =
                    entry_annotations_submit(&state, &bearer, key, &mut fresh).await;
                fresh["annotation"] = json!("new record");
                fresh["annotation_updates"] = entry_annotations_updates(&conflict, &["changed"]);
                let (status, result) =
                    entry_annotations_submit(&state, &bearer, key, &mut fresh).await;
                assert_eq!(status, StatusCode::CREATED, "{result}");
            }
            "policy" => {
                state
                    .surface_policy_store_for_test()
                    .transition(
                        &pool,
                        SurfacePolicyNameV2::SurfaceWarningAcknowledgement,
                        false,
                    )
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let (_, response) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/entries"),
            &bearer,
            Some(Uuid::now_v7()),
            Some(body),
        )
        .await;
        assert_eq!(
            response["code"],
            if scenario == "policy" {
                "surface_policy_changed"
            } else {
                "surface_matches_changed"
            },
            "{scenario}: {response}"
        );
    }
}

#[sqlx::test]
async fn surface_confirm_bugfix_confirms_all_suggested_dictionary_forms(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    create_v3_with_complete_forms(&state, &pool, &bearer).await;
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
            $1, 'kaikki:harbour:noun:test', 'harbour', 'noun', '[]'::jsonb,
            $2, $3, 'https://kaikki.org/dictionary/English/meaning/c/ch/harbour.html'
        )
        "#,
    )
    .bind(dataset_id)
    .bind(json!([{"form": "harbor", "tags": ["plural"]}]))
    .bind(json!([{"ipa": "/harbour/"}]))
    .execute(&pool)
    .await
    .unwrap();

    let (_, detection) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"schema_version":3,"language":"en","kind":"word","surface":"harbour"})),
    )
    .await;
    let body = json!({"schema_version":3,"detection_id":detection["detection_id"],"kind":"word",
        "headwords":{"mode":"unified","common":"harbour"},
        "confirmed_surface_match_token":detection["surface_match_page"]["surface_confirmation_token"]});
    assert!(
        body["confirmed_surface_match_token"].is_string(),
        "{detection}"
    );
    let (_, response) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(body),
    )
    .await;
    assert_eq!(
        response["code"], "annotation_conflict",
        "default dictionary forms must be confirmed during detection: {response}"
    );
}

#[sqlx::test]
async fn empty_draft_bugfix_http_detect_and_create_explain_existing_draft(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, tsz_rust::router(state))
            .await
            .unwrap()
    });
    let client = reqwest::Client::new();
    let mut existing_id = Value::Null;
    for attempt in 0..2 {
        let response = client.post(format!("http://{address}{ROOT}/detections"))
            .bearer_auth(&bearer).header("content-type", "application/json")
            .body(json!({"schema_version":3,"language":"en","kind":"word","surface":"emptydraftprobe"}).to_string())
            .send().await.unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let detection: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
        assert_eq!(detection["matches"], json!([]));
        assert_eq!(detection["requires_acknowledgement"], false);
        let response = client
            .post(format!("http://{address}{ROOT}/entries"))
            .bearer_auth(&bearer)
            .header("content-type", "application/json")
            .header("Idempotency-Key", Uuid::now_v7().to_string())
            .body(
                json!({"schema_version":3,"detection_id":detection["detection_id"],"kind":"word",
                "headwords":{"mode":"unified","common":"emptydraftprobe"}})
                .to_string(),
            )
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let result: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
        if attempt == 0 {
            assert_eq!(status, 201, "{result}");
            existing_id = result["word"]["id"].clone();
        } else {
            assert_eq!(status, 409, "{result}");
            assert_eq!(result["code"], "duplicate_word", "{result}");
            eprintln!(
                "empty draft baseline detection={detection}; create_status={status}; create={result}"
            );
            assert_eq!(
                detection["existing_draft_id"], existing_id,
                "detect must expose own empty draft"
            );
            assert_eq!(
                result["meta"]["word_id"], existing_id,
                "conflict must offer the same draft"
            );
        }
    }
    server.abort();
}

#[sqlx::test]
async fn empty_draft_bugfix_visibility_race_archive_and_resume(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = token(&state, seed_admin(&pool).await);
    let other = token(&state, seed_admin(&pool).await);
    // Detect before another request creates the skeleton: creation must return a resumable conflict.
    let (_, before) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"schema_version":3,"language":"en","kind":"word","surface":"harbour"})),
    )
    .await;
    assert!(before.get("existing_draft_id").is_none());
    let id = create_legacy_v3_empty_skeleton(&state, &bearer, "harbour").await;
    let (_, raced) = call(&state, Method::POST, &format!("{ROOT}/entries"), &bearer, Some(Uuid::now_v7()),
        Some(json!({"schema_version":3,"detection_id":before["detection_id"],"kind":"word","headwords":{"mode":"unified","common":"harbour"}}))).await;
    assert_eq!(raced["code"], "duplicate_word", "{raced}");
    assert_eq!(raced["meta"]["word_id"], id.to_string());
    // 别人的空草稿同样报出来（2026-09-08 口径）：此前这里是隐形的，外人只会拿到一个
    // 不说明理由的 duplicate_word，既不知道谁在建，也无从判断该等谁。
    let (_, visible) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &other,
        None,
        Some(json!({"schema_version":3,"language":"en","kind":"word","surface":"harbour"})),
    )
    .await;
    assert_eq!(visible["existing_draft_id"], id.to_string(), "{visible}");
    // 空骨架没有词形行，所以照旧没有 surface 命中可亮——报的是「已有人在建」而非命中。
    assert_eq!(visible["matches"], json!([]));
    let (_, denied) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &other,
        Some(Uuid::now_v7()),
        Some(json!({"schema_version":3,"detection_id":visible["detection_id"],"kind":"word"})),
    )
    .await;
    assert_eq!(denied["code"], "duplicate_word", "{denied}");
    assert_eq!(
        denied["meta"]["word_id"],
        id.to_string(),
        "外人也该拿到词条 ID，点进去是只读的：{denied}"
    );
    // Same surface in another kind must not expose a word draft.
    let (_, phrase) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"schema_version":3,"language":"en","kind":"phrase","surface":"harbour"})),
    )
    .await;
    assert!(phrase.get("existing_draft_id").is_none(), "{phrase}");
    let (status, original) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{original}");
    assert_eq!(original["word"]["id"], id.to_string());
    save_v3_forms_after_impact(
        &state,
        &bearer,
        &id.to_string(),
        1,
        "save",
        complete_v3_forms_fixture(),
    )
    .await;
    let (_, saved) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(json!({"schema_version":3,"language":"en","kind":"word","surface":"harbour"})),
    )
    .await;
    assert!(saved.get("existing_draft_id").is_none(), "{saved}");
    assert!(!saved["matches"].as_array().unwrap().is_empty());
    let (_, annotations) = call(&state, Method::POST, &format!("{ROOT}/entries"), &bearer, Some(Uuid::now_v7()),
        Some(json!({"schema_version":3,"detection_id":saved["detection_id"],"kind":"word",
            "headwords":{"mode":"unified","common":"harbour"},
            "confirmed_surface_match_token":saved["surface_match_page"]["surface_confirmation_token"]}))).await;
    assert_eq!(annotations["code"], "annotation_conflict", "{annotations}");
    let archived_id = create_legacy_v3_empty_skeleton(&state, &bearer, "emptyarchiveprobe").await;
    let (_, before_archive) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(
            json!({"schema_version":3,"language":"en","kind":"word","surface":"emptyarchiveprobe"}),
        ),
    )
    .await;
    assert_eq!(before_archive["existing_draft_id"], archived_id.to_string());
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/archive-batch"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({"entries":[{"id":archived_id,"base_revision":1,"base_lifecycle_revision":1}]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    let (_, absent) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/detections"),
        &bearer,
        None,
        Some(
            json!({"schema_version":3,"language":"en","kind":"word","surface":"emptyarchiveprobe"}),
        ),
    )
    .await;
    assert!(absent.get("existing_draft_id").is_none(), "{absent}");
    let (status, replacement) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(
            json!({"schema_version":3,"detection_id":before_archive["detection_id"],"kind":"word"}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "stale hint must not block after archive: {replacement}"
    );
    assert_ne!(replacement["word"]["id"], archived_id.to_string());
    // Final confirmed keys, not the obsolete original detection surface, own the skeleton.
    let final_body = entry_annotations_create_body(
        &state,
        &bearer,
        "emptyoldprobe",
        json!({"mode":"unified","common":"emptyfinalprobe"}),
    )
    .await;
    let (status, final_draft) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(final_body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{final_draft}");
    for surface in ["emptyoldprobe", "emptyfinalprobe"] {
        let (_, detection) = call(
            &state,
            Method::POST,
            &format!("{ROOT}/detections"),
            &bearer,
            None,
            Some(json!({"schema_version":3,"language":"en","kind":"word","surface":surface})),
        )
        .await;
        if surface == "emptyoldprobe" {
            assert!(detection.get("existing_draft_id").is_none(), "{detection}");
        } else {
            assert_eq!(detection["existing_draft_id"], final_draft["word"]["id"]);
        }
    }
}

/// 标注的修改权限：超管可以改任何词条，其他管理员只能改自己创建的。
#[sqlx::test]
async fn entry_annotation_edit_is_limited_to_creator_or_super_admin(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let owner = token(&state, seed_admin(&pool).await);
    let outsider = token(&state, seed_admin(&pool).await);
    let super_admin = token(
        &state,
        seed_admin_with_role(&pool, AdminRole::SuperAdmin).await,
    );
    let entry = create_v3_with_complete_forms(&state, &pool, &owner).await;
    let path = format!(
        "{ROOT}/entries/{}/annotation",
        entry["word"]["id"].as_str().unwrap()
    );

    let (status, denied) = call(
        &state,
        Method::PATCH,
        &path,
        &outsider,
        None,
        Some(json!({"annotation":"nope","base_annotation_revision":1})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    assert_eq!(denied["code"], "entry_annotation_forbidden", "{denied}");

    // 正向对照：创建者本人不受影响（防「一律拒绝」的空实现全绿）。
    let (status, saved) = call(
        &state,
        Method::PATCH,
        &path,
        &owner,
        None,
        Some(json!({"annotation":"1","base_annotation_revision":1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let (status, overridden) = call(
        &state,
        Method::PATCH,
        &path,
        &super_admin,
        None,
        Some(json!({"annotation":"2","base_annotation_revision":saved["annotation_revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "超管可以改任何词条：{overridden}");
    assert_eq!(overridden["annotation"], "2");
}

/// 超管不豁免「填满整组」：它的可写集合就是整组，所以建条撞名时每个组员都要给值。
///
/// 普通管理员只能改自己的标注；超管还必须补齐其他创建者的空标注草稿。
#[sqlx::test]
async fn entry_annotations_super_admin_must_fill_whole_group(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let owner_id = seed_admin(&pool).await;
    let owner = token(&state, owner_id);
    let super_admin = token(
        &state,
        seed_admin_with_role(&pool, AdminRole::SuperAdmin).await,
    );

    // 别人的未标注草稿同样进组，超管可在首次建条冲突中补齐。
    let first = create_v3_with_complete_forms(&state, &pool, &owner).await;
    let first_id = first["word"]["id"].as_str().unwrap().to_owned();

    let mut body = entry_annotations_create_body(
        &state,
        &super_admin,
        "harbour",
        json!({"mode":"unified","common":"harbour"}),
    )
    .await;
    let key = Uuid::now_v7();
    let (status, required) = entry_annotations_submit(&state, &super_admin, key, &mut body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{required}");
    assert_eq!(
        required["meta"]["annotation_conflict"]["reason"], "required",
        "{required}"
    );
    let entries = required["meta"]["annotation_conflict"]["entries"]
        .as_array()
        .unwrap();
    assert_eq!(entries.len(), 1, "{required}");
    assert_eq!(entries[0]["entry_id"], first_id);
    assert_eq!(entries[0]["created_by"], json!(owner_id));
    assert!(entries[0]["annotation"].is_null());

    // 只填自己那条：普通管理员到这步就建成了，超管不行。
    body["annotation"] = json!("2");
    body["annotation_updates"] = json!([]);
    let (status, still_required) =
        entry_annotations_submit(&state, &super_admin, key, &mut body).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "超管的可写集合是整组，只填自己那条不该放行：{still_required}"
    );
    assert_eq!(
        still_required["meta"]["annotation_conflict"]["reason"], "required",
        "{still_required}"
    );

    // 填满整组才建得成，且对方那条确实被写入——证明超管的写权限真的生效，
    // 而不是「碰巧没人校验」。
    body["annotation_updates"] = entry_annotations_updates(&required, &["1"]);
    let (status, created) = entry_annotations_submit(&state, &super_admin, key, &mut body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["word"]["annotation"], "2");
    let (status, peer) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{first_id}"),
        &owner,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{peer}");
    assert_eq!(
        peer["word"]["annotation"], "1",
        "超管应能写入别人的词条：{peer}"
    );
}

/// 别人的草稿也参与分组与查重，但普通管理员无权修改。
#[sqlx::test]
async fn entry_annotations_other_admins_drafts_are_read_only_and_unique(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let owner = token(&state, seed_admin(&pool).await);
    let outsider = token(&state, seed_admin(&pool).await);
    let first = create_v3_with_complete_forms(&state, &pool, &owner).await;
    // owner 那条已经带标注，排除「因为对方没标注才不要求」的解释。
    let (status, labeled) = call(
        &state,
        Method::PATCH,
        &format!(
            "{ROOT}/entries/{}/annotation",
            first["word"]["id"].as_str().unwrap()
        ),
        &owner,
        None,
        Some(json!({"annotation":"1","base_annotation_revision":1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{labeled}");

    let mut body = entry_annotations_create_body(
        &state,
        &outsider,
        "harbour",
        json!({"mode":"unified","common":"harbour"}),
    )
    .await;
    let key = Uuid::now_v7();
    let (status, required) = entry_annotations_submit(&state, &outsider, key, &mut body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{required}");
    assert_eq!(required["code"], "annotation_conflict");
    assert_eq!(
        required["meta"]["annotation_conflict"]["entries"][0]["entry_id"],
        first["word"]["id"]
    );
    body["annotation"] = json!("1");
    let (status, duplicate) = entry_annotations_submit(&state, &outsider, key, &mut body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");
    assert_eq!(
        duplicate["meta"]["annotation_conflict"]["reason"],
        "duplicate"
    );
    body["annotation_updates"] = entry_annotations_updates(&required, &["3"]);
    let (status, denied) = entry_annotations_submit(&state, &outsider, key, &mut body).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    body["annotation"] = json!("2");
    body["annotation_updates"] = json!([]);
    let (status, created) = entry_annotations_submit(&state, &outsider, key, &mut body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["word"]["annotation"], "2");
    let (status, duplicate) = call(
        &state,
        Method::PATCH,
        &format!(
            "{ROOT}/entries/{}/annotation",
            created["word"]["id"].as_str().unwrap()
        ),
        &outsider,
        None,
        Some(json!({"annotation":"1","base_annotation_revision":1})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");
    assert_eq!(
        duplicate["meta"]["annotation_conflict"]["reason"],
        "duplicate"
    );
}

#[sqlx::test]
async fn text_links_persist_both_english_fields_publish_and_clear(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin = seed_admin(&pool).await;
    let bearer = token(&state, admin);
    let (target, publication_id) =
        create_published_v3_phrase(&state, &pool, &bearer, "mother up", json!([])).await;
    let component = resolved_component_json(&target, publication_id, "uk", "mother");
    let phrase_draft = create_v3_phrase_with_sense_components(
        &state,
        &bearer,
        "mother phrase",
        json!([component.clone()]),
    )
    .await;
    let (status, phrase) = publish_ready_v3(&state, &bearer, &phrase_draft).await;
    assert_eq!(status, StatusCode::CREATED, "{phrase}");
    let phrase_publication = current_publication_id(
        &pool,
        Uuid::parse_str(phrase["word"]["id"].as_str().unwrap()).unwrap(),
    )
    .await;
    let source =
        create_ready_v3_draft_with_sentences(&state, &pool, &bearer, &["A mother sentence."]).await;
    let mut link = resolved_component_json(&target, publication_id, "uk", "mother");
    for field in [
        "state",
        "literal",
        "target_dialect",
        "target_form_type",
        "target_headword",
        "target_gloss",
    ] {
        link.as_object_mut().unwrap().remove(field);
    }
    link["source_segments"] = json!([{"start":2,"end":8,"surface":"mother"}]);
    let mut meanings = writable_v3_meanings(&source);
    meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["text_links"] =
        json!([link.clone()]);
    let mut definition_link = link.clone();
    definition_link["id"] = json!(Uuid::now_v7());
    definition_link["via_phrase"] = json!({"word_id":phrase["word"]["id"],"publication_id":phrase_publication,"sense_id":phrase["word"]["meanings"]["pos"][0]["senses"][0]["id"],"component_id":component["id"]});
    let grammar_id = meanings["pos"][0]["grammar_structures"][0]["id"].clone();
    meanings["pos"][0]["senses"][0]["definitions"].as_array_mut().unwrap().push(json!({
        "id":Uuid::now_v7(), "level":"B1", "grammar_structure_id":grammar_id, "definition_mode":"en_sentence",
        "content":{"mode":"unified","common":{"id":Uuid::now_v7(),"origin":"manual","value":rich_text("A mother definition."),"text_links":[definition_link]}}
    }));
    for (field, forged) in [
        ("target_form_id", json!(Uuid::now_v7())),
        ("target_word_id", source["word"]["id"].clone()),
        ("target_gloss", json!("伪造快照")),
    ] {
        let mut invalid = meanings.clone();
        invalid["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["text_links"][0]
            [field] = forged;
        let (status, problem) = save_v3_meanings_raw(
            &state,
            &bearer,
            source["word"]["id"].as_str().unwrap(),
            source["word"]["revision"].as_i64().unwrap(),
            invalid,
        )
        .await;
        assert!(status.is_client_error(), "{field}: {problem}");
    }
    let mut invalid = meanings.clone();
    invalid["pos"][0]["senses"][0]["definitions"][1]["content"]["common"]["text_links"][0]["via_phrase"]
        ["component_id"] = json!(Uuid::now_v7());
    let (status, problem) = save_v3_meanings_raw(
        &state,
        &bearer,
        source["word"]["id"].as_str().unwrap(),
        source["word"]["revision"].as_i64().unwrap(),
        invalid,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    let saved = save_v3_meanings(&state, &bearer, &source, meanings).await;
    let entry_id = saved["word"]["id"].as_str().unwrap();
    let check = |body: &Value| {
        let sense = &body["word"]["meanings"]["pos"][0]["senses"][0];
        assert_eq!(
            sense["sentences"][0]["en_text"]["common"]["text_links"][0]["target_word_id"],
            target["word"]["id"]
        );
        assert_eq!(
            sense["definitions"][1]["content"]["common"]["text_links"][0]["target_word_id"],
            target["word"]["id"]
        );
        assert_eq!(
            sense["sentences"][0]["en_text"]["common"]["text_links"][0]["target_headword"],
            target["word"]["presentation"]["label"]
        );
    };
    check(&saved);
    assert!(
        saved["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["associations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["origin"] == "manual" && a["target_word_id"] == target["word"]["id"])
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
    check(&reread);
    let (status, published) = publish_ready_v3(&state, &bearer, &saved).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    check(&published);
    let original_publication =
        current_publication_id(&pool, Uuid::parse_str(entry_id).unwrap()).await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publication_sense_refs WHERE entry_id=$1 AND reference_kind='text_link'")
        .bind(Uuid::parse_str(entry_id).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(count, 3);

    // 旧客户端省略字段仅在正文未变时保留，显式 [] 则清空。
    let mut omitted = writable_v3_meanings(&published);
    omitted["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]
        .as_object_mut()
        .unwrap()
        .remove("text_links");
    omitted["pos"][0]["senses"][0]["definitions"][1]["content"]["common"]
        .as_object_mut()
        .unwrap()
        .remove("text_links");
    let preserved = save_v3_meanings(&state, &bearer, &published, omitted.clone()).await;
    check(&preserved);
    omitted["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["value"] =
        rich_text("Changed mother sentence.");
    let (status, problem) = save_v3_meanings_raw(
        &state,
        &bearer,
        entry_id,
        preserved["word"]["revision"].as_i64().unwrap(),
        omitted.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    omitted["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["text_links"] = json!([]);
    omitted["pos"][0]["senses"][0]["definitions"][1]["content"]["common"]["text_links"] = json!([]);
    let cleared = save_v3_meanings(&state, &bearer, &preserved, omitted).await;
    assert!(cleared["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["text_links"].is_null());
    assert!(
        !cleared["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["associations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["origin"] == "manual")
    );
    // 发布清除后的版本，再切回旧版本，人工关联与快照一并恢复。
    let (status, cleared_publication) = publish_ready_v3(&state, &bearer, &cleared).await;
    assert_eq!(status, StatusCode::CREATED, "{cleared_publication}");
    let (status, restored) = activate_v3_history(
        &state,
        &bearer,
        Uuid::parse_str(entry_id).unwrap(),
        original_publication,
        cleared_publication["word"]["revision"].as_i64().unwrap(),
        cleared_publication["word"]["lifecycle_revision"]
            .as_i64()
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(
        current_publication_id(&pool, Uuid::parse_str(entry_id).unwrap()).await,
        original_publication
    );
    let (status, historical) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}/publications/{original_publication}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{historical}");
    assert_eq!(historical["publication"]["is_current"], true);
    check(&historical["publication"]);
    // 激活历史发布只切换生效指针，保留尚在编辑的草稿。
    assert!(restored["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["text_links"].is_null());
    // 经由短语只有正文引用，归档必须被 text_link 守卫阻止。
    let (status, blocked) = call(&state, Method::POST, &format!("{ROOT}/entries/{}/archive",phrase["word"]["id"].as_str().unwrap()), &bearer, Some(Uuid::now_v7()),
        Some(json!({"base_revision":phrase["word"]["revision"],"base_lifecycle_revision":phrase["word"]["lifecycle_revision"]}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
    assert_eq!(blocked["code"], "entry_has_inbound_publication_refs");
    assert_eq!(
        blocked["meta"]["reference_locations"][0]["reference_kind"],
        "text_link"
    );
}

/// 词形步保存（save 意图）并带上 impact 端点给的确认 token；只关心 kind 校验，不走 complete。
async fn save_v3_forms_draft(
    state: &AppState,
    bearer: &str,
    entry_id: &str,
    base_revision: i64,
    content: Value,
) -> (StatusCode, Value) {
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
    let mut forms_input = json!({
        "schema_version": 3,
        "base_revision": base_revision,
        "intent": "save",
        "content": content
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
    call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        bearer,
        None,
        Some(forms_input),
    )
    .await
}

async fn create_v3_skeleton(state: &AppState, bearer: &str, kind: &str, surface: &str) -> String {
    let (status, detection) = call(
        state,
        Method::POST,
        &format!("{ROOT}/detections"),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": kind,
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
            "kind": kind,
            "headwords": { "mode": "unified", "common": surface }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created["word"]["id"].as_str().unwrap().to_owned()
}

#[sqlx::test]
async fn v3_entry_pos_must_match_entry_kind(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = token(&state, admin_id);
    sqlx::query(
        r#"
        INSERT INTO catalog.parts_of_speech (
            id, kind, code, name_zh, name_en, abbreviation, short_name_zh, full_name_en, sort_order
        ) VALUES ($1, 'phrase', 'phrase_noun', '名词', 'NOUN', 'n.', '名词', 'noun', 10)
        "#,
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .expect("插入短语名词应成功");

    // 短语词条挂单词词性：拦在词形保存，问题锚在 pos 节点上。
    let phrase_id = create_v3_skeleton(&state, &bearer, "phrase", "a piece of cake").await;
    let mut content = v3_forms_fixture_for("a piece of cake");
    let pos_id = content["pos"][0]["pos_id"].clone();
    let (status, body) = save_v3_forms_draft(&state, &bearer, &phrase_id, 1, content.clone()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        has_issue(&body, "part_of_speech_kind_mismatch"),
        "应报 part_of_speech_kind_mismatch：{body}"
    );
    let issue = body["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "part_of_speech_kind_mismatch")
        .unwrap();
    assert_eq!(issue["node_id"], pos_id);
    assert_eq!(issue["field"], "pos");
    let stored: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_pos WHERE entry_id = $1")
            .bind(Uuid::parse_str(&phrase_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, 0, "校验失败不得写入 entry_pos");

    // 改选短语词性后保存成功，entry_pos 记下词条 kind。
    content["pos"][0]["pos"] = json!("phrase_noun");
    let (status, body) = save_v3_forms_draft(&state, &bearer, &phrase_id, 1, content).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entry_kinds: Vec<String> =
        sqlx::query_scalar("SELECT entry_kind FROM lexicon.entry_pos WHERE entry_id = $1")
            .bind(Uuid::parse_str(&phrase_id).unwrap())
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(entry_kinds, vec!["phrase".to_owned()]);

    // 反向同样拦：单词词条挂短语词性。
    seed_dictionary_word(&pool, "harbour").await;
    let word_id = create_v3_skeleton(&state, &bearer, "word", "harbour").await;
    let mut content = v3_forms_fixture_for("harbour");
    content["pos"][0]["pos"] = json!("phrase_noun");
    let (status, body) = save_v3_forms_draft(&state, &bearer, &word_id, 1, content).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        has_issue(&body, "part_of_speech_kind_mismatch"),
        "单词词条挂短语词性也应报 part_of_speech_kind_mismatch：{body}"
    );
}

// ===== 短语词条只能挂短语词性（2026-09-12 起）；测试库没有短语词性种子，用到时补一个 =====

const PHRASE_NOUN: &str = "phrase_noun";

async fn seed_phrase_noun(pool: &PgPool) {
    sqlx::query(
        r#"
        INSERT INTO catalog.parts_of_speech (
            id, kind, code, name_zh, name_en, abbreviation, short_name_zh, full_name_en, sort_order
        ) VALUES ($1, 'phrase', $2, '名词', 'NOUN', 'n.', '名词', 'noun', 10)
        ON CONFLICT (code) DO NOTHING
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(PHRASE_NOUN)
    .execute(pool)
    .await
    .expect("补短语名词种子应成功");
}

/// 与 complete_v3_forms_fixture 同形，只是挂短语词性、两侧拼写换成短语本身。
fn phrase_forms_fixture(surface: &str) -> Value {
    let mut forms = complete_v3_forms_fixture();
    forms["pos"][0]["pos"] = json!(PHRASE_NOUN);
    for form in forms["pos"][0]["forms"].as_array_mut().unwrap() {
        form["regional_variants"]["uk"]["spelling"] = json!(surface);
        form["regional_variants"]["us"]["spelling"] = json!(surface);
    }
    forms
}

/// 短语名词下没配细分词性，词义的 sub_pos 留空（空串表示不选）。
fn phrase_meanings_fixture(pos_id: Value) -> Value {
    let mut meanings = complete_v3_meanings_fixture(pos_id);
    for sense in meanings["pos"][0]["senses"].as_array_mut().unwrap() {
        sense["sub_pos"] = json!("");
    }
    meanings
}
