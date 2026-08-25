//! Cross-version relation and sentence-context consumers for migrated V3 targets.

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    config::SmartLexiconV3Flags,
    platform,
    state::AppState,
};

const ROOT: &str = "/api/v1/admin/lexicon";

struct MigratedTarget {
    entry_id: Uuid,
    sense_id: Uuid,
    v2_publication_id: Uuid,
    v3_publication_id: Uuid,
    v3_snapshot: Value,
}

#[derive(Debug, PartialEq)]
struct SourceWitness {
    revision: i64,
    updated_at: DateTime<Utc>,
    meanings: Value,
    relation_count: i64,
    node_count: i64,
}

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
            phone: format!("v3-relation-consumer-{}", id.simple()),
            display_name: "V3 relation consumer tester".to_owned(),
            password_hash: "hashed-password".to_owned(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin should succeed");
    id
}

fn bearer(state: &AppState, admin_id: Uuid) -> String {
    state
        .admin_token_manager
        .generate(admin_id, AdminRole::Admin.as_str())
        .expect("test token should be generated")
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
    if let Some(key) = idempotency_key {
        builder = builder.header("Idempotency-Key", key.to_string());
    }
    let body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        }
        None => Body::empty(),
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
                "response should be JSON: {error}; body={}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, body)
}

fn rich_text(text: &str) -> Value {
    json!({"version": 1, "text": text, "spans": [], "liaisons": []})
}

async fn seed_dictionary_word(pool: &PgPool, word: &str) {
    let dataset_id: i64 = if let Some(dataset_id) =
        sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status = 'active'")
            .fetch_optional(pool)
            .await
            .unwrap()
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
        .bind(format!("v3-relation-{word}"))
        .fetch_one(pool)
        .await
        .unwrap()
    };
    sqlx::query(
        r#"
        INSERT INTO dictionary.terms (
            dataset_id, normalized_term, term, kind, pos, status,
            sense_count, filtered_cold_sense_count, region_family
        ) VALUES ($1, $2, $2, 'word', ARRAY['noun'], 'accepted', 1, 0, 'common_unmarked')
        "#,
    )
    .bind(dataset_id)
    .bind(word)
    .execute(pool)
    .await
    .unwrap();
}

async fn create_ready_v2_source(
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
    assert_eq!(status, StatusCode::OK, "{detection}");
    let mut create_input = json!({
        "schema_version": 2,
        "detection_id": detection["detection_id"],
        "headwords": detection["builtin_dictionary"]["headwords"]
    });
    if let Some(token) =
        detection["smart_dictionary"]["surface_match_page"]["surface_confirmation_token"].as_str()
    {
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

    let mut forms = created["word"]["forms"].clone();
    let base_variants = forms["pos"][0]["base_form"]["variants"]
        .as_array()
        .unwrap()
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
        "variants": base_variants.iter().map(|variant| json!({
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
        })).collect::<Vec<_>>()
    }]);
    let forms_input = json!({
        "base_revision": 1,
        "intent": "complete",
        "content": forms
    });
    let (mut status, mut saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        bearer,
        None,
        Some(forms_input.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT {
        let token = saved["meta"]["surface_match_page"]["surface_confirmation_token"]
            .as_str()
            .unwrap();
        let mut confirmed = forms_input;
        confirmed["confirmed_surface_match_token"] = json!(token);
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

    let mut meanings = saved["word"]["meanings"].clone();
    meanings["sense_groups"][0]["name_zh"] = json!("来源义");
    meanings["sense_groups"][0]["name_en"] = json!("source sense");
    meanings["pos"][0]["grammar_structures"][0]["variants"][0]["content"] =
        rich_text("used as a noun");
    meanings["pos"][0]["senses"][0]["sub_pos"] = json!("N-COUNT");
    meanings["pos"][0]["senses"][0]["frequency"] = json!("50");
    meanings["pos"][0]["senses"][0]["definitions"][0]["content"] = rich_text("来源释义");
    meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["value"] =
        rich_text("A source example.");
    meanings["pos"][0]["senses"][0]["sentences"][0]["zh_text"] = rich_text("来源例句。");
    let (status, saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        bearer,
        None,
        Some(json!({
            "base_revision": 2,
            "intent": "complete",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved
}

fn v3_forms(surface: &str) -> Value {
    let pos_id = Uuid::now_v7();
    let form_id = Uuid::now_v7();
    json!({
        "pos": [{
            "pos_id": pos_id,
            "pos": "noun",
            "forms": [{
                "id": form_id,
                "form_type": "base",
                "regional_variants": {
                    "mode": "common",
                    "common": {
                        "id": Uuid::now_v7(),
                        "dialect": "common",
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

async fn create_v3_source_forms(
    state: &AppState,
    pool: &PgPool,
    bearer: &str,
    surface: &str,
) -> Value {
    seed_dictionary_word(pool, surface).await;
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
    let input = json!({
        "schema_version": 3,
        "base_revision": 1,
        "intent": "complete",
        "content": v3_forms(surface)
    });
    let (mut status, mut saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        bearer,
        None,
        Some(input.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT {
        let mut confirmed = input;
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

fn v3_reference_meanings(
    source_entry_id: Uuid,
    pos_id: Uuid,
    target_entry_id: Uuid,
    target_sense_id: Uuid,
) -> Value {
    let sense_group_id = Uuid::now_v7();
    let grammar_id = Uuid::now_v7();
    let source_sense_id = Uuid::now_v7();
    json!({
        "sense_groups": [{
            "id": sense_group_id,
            "name_zh": "来源义",
            "name_en": "source sense"
        }],
        "pos": [{
            "pos_id": pos_id,
            "grammar_structures": [{
                "id": grammar_id,
                "variants": [{
                    "id": Uuid::now_v7(),
                    "dialect": "common",
                    "content": rich_text("used as a noun")
                }]
            }],
            "senses": [{
                "id": source_sense_id,
                "sub_pos": "N-COUNT",
                "level": "A1",
                "sense_group_id": sense_group_id,
                "frequency": "50",
                "depends_on_context": false,
                "definitions": [{
                    "definition_mode": "zh_definition",
                    "id": Uuid::now_v7(),
                    "content_id": Uuid::now_v7(),
                    "level": "A1",
                    "grammar_structure_id": grammar_id,
                    "content": rich_text("来源释义")
                }],
                "sentences": [{
                    "id": Uuid::now_v7(),
                    "level": "A1",
                    "en_text": {
                        "mode": "unified",
                        "common": {
                            "id": Uuid::now_v7(),
                            "value": rich_text("A source example."),
                            "origin": "manual"
                        }
                    },
                    "zh_text_id": Uuid::now_v7(),
                    "zh_text": rich_text("来源例句。"),
                    "links": [{
                        "word_id": source_entry_id,
                        "sense_id": source_sense_id,
                        "role": "focus"
                    }, {
                        "word_id": target_entry_id,
                        "sense_id": target_sense_id,
                        "role": "context"
                    }]
                }],
                "relations": [{
                    "id": Uuid::now_v7(),
                    "relation": "synonym",
                    "target_word_id": target_entry_id,
                    "target_sense_id": target_sense_id,
                    "score": "95.00"
                }]
            }]
        }]
    })
}

fn v2_reference_meanings(source: &Value, target: &MigratedTarget) -> Value {
    let mut meanings = source["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["relations"] = json!([{
        "id": Uuid::now_v7(),
        "relation": "synonym",
        "target_word_id": target.entry_id,
        "target_sense_id": target.sense_id,
        "target_headword": "forged",
        "target_gloss": "forged",
        "score": "95.00"
    }]);
    meanings["pos"][0]["senses"][0]["sentences"][0]["links"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "word_id": target.entry_id,
            "sense_id": target.sense_id,
            "role": "context"
        }));
    meanings
}

fn target_forms() -> Value {
    let pos_id = Uuid::now_v7();
    let form_id = Uuid::now_v7();
    json!({
        "pos": [{
            "pos_id": pos_id,
            "pos": "noun",
            "forms": [{
                "id": form_id,
                "form_type": "base",
                "regional_variants": {
                    "mode": "uk_us",
                    "uk": {
                        "id": Uuid::now_v7(),
                        "dialect": "uk",
                        "spelling": "harbour",
                        "origin": "manual",
                        "pronunciations": []
                    },
                    "us": {
                        "id": Uuid::now_v7(),
                        "dialect": "us",
                        "spelling": "harbor",
                        "origin": "manual",
                        "pronunciations": []
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

fn target_meanings(pos_id: Uuid, sense_id: Uuid, gloss: &str) -> Value {
    json!({
        "sense_groups": [],
        "pos": [{
            "pos_id": pos_id,
            "grammar_structures": [],
            "senses": [{
                "id": sense_id,
                "sub_pos": "N-COUNT",
                "level": "A1",
                "frequency": "50",
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
    })
}

async fn seed_migrated_target(
    pool: &PgPool,
    admin_id: Uuid,
    legacy_headword: &str,
) -> MigratedTarget {
    let entry_id = Uuid::now_v7();
    let sense_id = Uuid::now_v7();
    let forms = target_forms();
    let pos_id = Uuid::parse_str(forms["pos"][0]["pos_id"].as_str().unwrap()).unwrap();
    let draft_meanings = target_meanings(pos_id, sense_id, "新释义");
    let now = Utc::now();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entries (
            id, content_schema_version, language, kind, revision,
            headword_mode, source_dialect, detection_snapshot,
            created_by_admin_id, updated_by_admin_id, created_at, updated_at
        ) VALUES ($1, 3, 'en', 'word', 7, 'unified', NULL, '{}', $2, $2, $3, $3)
        "#,
    )
    .bind(entry_id)
    .bind(admin_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_headwords (
            id, entry_id, dialect, headword, normalized_headword,
            normalization_version, origin
        ) VALUES ($1, $2, 'common', $3, $3, 1, 'manual')
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(entry_id)
    .bind(legacy_headword)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_editor_projection (entry_id, forms, meanings, rebuilt_revision)
        VALUES ($1, $2, $3, 7)
        "#,
    )
    .bind(entry_id)
    .bind(&forms)
    .bind(&draft_meanings)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_presentation_projection (
            entry_id, content_schema_version, source_revision,
            label, matched_surfaces, strategy_version
        ) VALUES ($1, 3, 7, 'harbour / harbor', ARRAY['harbour','harbor'], 'relation-test-v1')
        "#,
    )
    .bind(entry_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO lexicon.nodes (id, entry_id, node_type) VALUES ($1, $2, 'sense')")
        .bind(sense_id)
        .bind(entry_id)
        .execute(pool)
        .await
        .unwrap();
    for step in ["basics", "forms", "meanings"] {
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_step_progress (
                entry_id, step, completed_revision, content_hash
            ) VALUES ($1, $2, 7, $3)
            "#,
        )
        .bind(entry_id)
        .bind(step)
        .bind(format!("{step}-hash").into_bytes())
        .execute(pool)
        .await
        .unwrap();
    }

    let v2_publication_id = Uuid::now_v7();
    let v3_publication_id = Uuid::now_v7();
    let v2_meanings = target_meanings(pos_id, sense_id, "旧释义");
    let v2_snapshot = json!({
        "schema_version": 2,
        "id": entry_id,
        "language": "en",
        "kind": "word",
        "status": "published",
        "revision": 7,
        "lifecycle_revision": 1,
        "published_revision": 7,
        "has_unpublished_changes": false,
        "headwords": {"mode": "unified", "common": legacy_headword},
        "detection_snapshot": {
            "detection_id": Uuid::now_v7(),
            "request": {"language": "en", "headword": legacy_headword},
            "normalized_headword": legacy_headword,
            "entry_kind": "word",
            "matched_dialect": "common",
            "builtin_dictionary_status": "matched",
            "smart_dictionary_status": "clear",
            "headwords": {"mode": "unified", "common": legacy_headword},
            "suggested_pos": ["noun"],
            "detected_at": now
        },
        "forms": {"pos": []},
        "meanings": v2_meanings,
        "completed_steps": ["basics", "forms", "meanings"],
        "max_reachable_step": "preview",
        "created_by": admin_id,
        "created_at": now,
        "updated_at": now,
        "published_at": now
    });
    let v3_snapshot = json!({
        "schema_version": 3,
        "id": entry_id,
        "language": "en",
        "kind": "word",
        "status": "published",
        "revision": 7,
        "lifecycle_revision": 1,
        "published_revision": 7,
        "has_unpublished_changes": false,
        "presentation": {
            "label": "harbour / harbor",
            "matched_surfaces": ["harbour", "harbor"],
            "strategy_version": "relation-test-v1"
        },
        "capabilities": {
            "publication": {"mode": "migration_canary", "whitelisted": true},
            "pronunciation_normalization_version": "nfkc_trim_lower_v1"
        },
        "forms": forms,
        "meanings": draft_meanings,
        "compatibility": {
            "legacy_headwords": {"mode": "unified", "common": legacy_headword}
        },
        "completed_steps": ["basics", "forms", "meanings"],
        "max_reachable_step": "preview",
        "created_by": admin_id,
        "created_at": now,
        "updated_at": now,
        "published_at": now
    });
    for (publication_id, number, schema_version, snapshot, hash) in [
        (
            v2_publication_id,
            1_i32,
            2_i16,
            &v2_snapshot,
            vec![2_u8; 32],
        ),
        (
            v3_publication_id,
            2_i32,
            3_i16,
            &v3_snapshot,
            vec![3_u8; 32],
        ),
    ] {
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_publications (
                id, entry_id, publication_number, source_revision,
                content_schema_version, snapshot, snapshot_hash,
                published_by_admin_id, published_at
            ) VALUES ($1, $2, $3, 7, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(publication_id)
        .bind(entry_id)
        .bind(number)
        .bind(schema_version)
        .bind(snapshot)
        .bind(hash)
        .bind(admin_id)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_publication_nodes (
                publication_id, entry_id, node_id, node_type
            ) VALUES ($1, $2, $3, 'sense')
            "#,
        )
        .bind(publication_id)
        .bind(entry_id)
        .bind(sense_id)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        r#"
        UPDATE lexicon.entries
        SET current_publication_id = $2, draft_based_on_publication_id = $2
        WHERE id = $1
        "#,
    )
    .bind(entry_id)
    .bind(v2_publication_id)
    .execute(pool)
    .await
    .unwrap();

    let batch_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_migration_batches (
            id, status, selection_digest, manifest_digest,
            requested_by_admin_id, request_id,
            approved_by_admin_id, approval_request_id, approved_at,
            scanned_count, eligible_count, applied_count, finished_at
        ) VALUES (
            $1, 'verified', $2, $3,
            $4, $5, $4, $6, $7,
            1, 1, 1, $7
        )
        "#,
    )
    .bind(batch_id)
    .bind(vec![4_u8; 32])
    .bind(vec![5_u8; 32])
    .bind(admin_id)
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_migration_entries (
            batch_id, entry_id, status, source_revision,
            source_current_publication_id, source_publications_digest,
            source_pos_modes, source_forms, source_meanings,
            source_draft_surfaces, expected_forms, expected_presentation,
            expected_digest, applied_digest, applied_at, verified_at
        ) VALUES (
            $1, $2, 'verified', 7,
            $3, $4,
            '{}', '{}', '{}',
            '[]', '{}', '{}',
            $5, $6, $7, $7
        )
        "#,
    )
    .bind(batch_id)
    .bind(entry_id)
    .bind(v2_publication_id)
    .bind(vec![6_u8; 32])
    .bind(vec![7_u8; 32])
    .bind(vec![8_u8; 32])
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_entry_state (
            entry_id, origin, migration_batch_id, source_publication_id,
            source_revision, publication_canary_enabled
        ) VALUES ($1, 'migrated_v2', $2, $3, 7, TRUE)
        "#,
    )
    .bind(entry_id)
    .bind(batch_id)
    .bind(v2_publication_id)
    .execute(pool)
    .await
    .unwrap();

    MigratedTarget {
        entry_id,
        sense_id,
        v2_publication_id,
        v3_publication_id,
        v3_snapshot,
    }
}

async fn seed_current_v2_related_surface(pool: &PgPool, target: &MigratedTarget, surface: &str) {
    for dialect_scope in ["uk", "us"] {
        sqlx::query(
            r#"
            INSERT INTO lexicon.surface_sources (
                entry_id, source_id, source_kind, language, entry_kind,
                dialect, dialect_scope, surface, normalized_surface,
                normalization_version, source_revision, is_deleted,
                content_scope, publication_id, content_schema_version
            ) VALUES (
                $1, $2, 'headword', 'en', 'word',
                'common', $3, $4, $4,
                1, 7, FALSE,
                'current_publication', $5, 2
            )
            "#,
        )
        .bind(target.entry_id)
        .bind(format!("entry:{}:headword:common", target.entry_id))
        .bind(dialect_scope)
        .bind(surface)
        .bind(target.v2_publication_id)
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn activate_related_publication(
    state: &AppState,
    bearer: &str,
    entry_id: Uuid,
    publication_id: Uuid,
) -> Value {
    let (status, current) = call(
        state,
        Method::GET,
        &format!("{ROOT}/entries/{entry_id}"),
        bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{current}");
    let mut body = json!({
        "schema_version": 3,
        "base_revision": current["word"]["revision"],
        "base_lifecycle_revision": current["word"]["lifecycle_revision"]
    });
    let path = format!("{ROOT}/entries/{entry_id}/publications/{publication_id}/activate");
    let key = Uuid::now_v7();
    let (mut status, mut activated) = call(
        state,
        Method::POST,
        &path,
        bearer,
        Some(key),
        Some(body.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT
        && activated["code"] == "surface_match_acknowledgement_required"
    {
        body["confirmed_surface_match_token"] =
            activated["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
        (status, activated) = call(state, Method::POST, &path, bearer, Some(key), Some(body)).await;
    }
    assert_eq!(status, StatusCode::OK, "{activated}");
    activated
}

async fn source_witness(pool: &PgPool, entry_id: Uuid) -> SourceWitness {
    let (revision, updated_at): (i64, DateTime<Utc>) =
        sqlx::query_as("SELECT revision, updated_at FROM lexicon.entries WHERE id = $1")
            .bind(entry_id)
            .fetch_one(pool)
            .await
            .unwrap();
    let meanings: Value = sqlx::query_scalar(
        "SELECT meanings FROM lexicon.entry_editor_projection WHERE entry_id = $1",
    )
    .bind(entry_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let relation_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.relations WHERE entry_id = $1")
            .bind(entry_id)
            .fetch_one(pool)
            .await
            .unwrap();
    let node_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.nodes WHERE entry_id = $1")
            .bind(entry_id)
            .fetch_one(pool)
            .await
            .unwrap();
    SourceWitness {
        revision,
        updated_at,
        meanings,
        relation_count,
        node_count,
    }
}

fn assert_relation_snapshot(word: &Value, headword: &str, gloss: &str) {
    let relation = &word["word"]["meanings"]["pos"][0]["senses"][0]["relations"][0];
    assert_eq!(relation["target_headword"], headword);
    assert_eq!(relation["target_gloss"], gloss);
}

fn has_issue(body: &Value, code: &str) -> bool {
    ["issues", "field_issues"].iter().any(|field| {
        body[*field]
            .as_array()
            .is_some_and(|issues| issues.iter().any(|issue| issue["code"] == code))
    })
}

async fn save_v2_example_sentence(
    state: &AppState,
    bearer: &str,
    word: &Value,
    text: &str,
) -> Value {
    let entry_id = word["word"]["id"].as_str().unwrap();
    let mut meanings = word["word"]["meanings"].clone();
    meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["value"] = rich_text(text);
    let (status, saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/meanings"),
        bearer,
        None,
        Some(json!({
            "base_revision": word["word"]["revision"],
            "intent": "complete",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved
}

async fn publish_v2_confirming(state: &AppState, bearer: &str, word: &Value) -> Value {
    let entry_id = word["word"]["id"].as_str().unwrap();
    let path = format!("{ROOT}/entries/{entry_id}/publications");
    let key = Uuid::now_v7();
    let mut body = json!({"base_revision": word["word"]["revision"]});
    let (mut status, mut published) = call(
        state,
        Method::POST,
        &path,
        bearer,
        Some(key),
        Some(body.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT
        && published["code"] == "surface_match_acknowledgement_required"
    {
        body["confirmed_surface_match_token"] =
            published["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
        (status, published) = call(state, Method::POST, &path, bearer, Some(key), Some(body)).await;
    }
    assert_eq!(status, StatusCode::CREATED, "{published}");
    published
}

#[sqlx::test]
async fn related_search_follows_mixed_v2_v3_current_publications_and_cursor(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("test Redis connection should succeed");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let switching = seed_migrated_target(&pool, admin_id, "legacyharbour").await;
    let remaining_v2 = seed_migrated_target(&pool, admin_id, "harbourside").await;
    seed_current_v2_related_surface(&pool, &switching, "legacyharbour").await;
    seed_current_v2_related_surface(&pool, &remaining_v2, "harbourside").await;

    let (status, initial) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=legacyharbour&kind=word&match_mode=exact&page_size=20"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{initial}");
    assert_eq!(initial["total"], 1);
    assert!(
        initial["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result["schema_version"] == 2),
        "{initial}"
    );

    let activated = activate_related_publication(
        &state,
        &bearer,
        switching.entry_id,
        switching.v3_publication_id,
    )
    .await;
    assert_eq!(activated["word"]["schema_version"], 3);

    let read_disabled = state
        .clone()
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags {
            read: false,
            ..SmartLexiconV3Flags::all_enabled()
        });
    let (status, hidden_v3) = call(
        &read_disabled,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=harbour&kind=word&match_mode=exact&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hidden_v3}");
    assert_eq!(
        hidden_v3,
        json!({"results": [], "total": 0, "next_cursor": null})
    );
    let (status, visible_v2) = call(
        &read_disabled,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=harbourside&kind=word&match_mode=exact&page_size=20"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{visible_v2}");
    assert_eq!(visible_v2["total"], 1);
    assert_eq!(visible_v2["results"][0]["schema_version"], 2);

    let (status, exact_uk) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=harbour&kind=word&match_mode=exact&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exact_uk}");
    assert_eq!(exact_uk["total"], 1, "{exact_uk}");
    let result = &exact_uk["results"][0];
    assert_eq!(result["schema_version"], 3);
    assert_eq!(result["entry_id"], switching.entry_id.to_string());
    assert_eq!(result["kind"], "word");
    assert_eq!(result["presentation"]["label"], "harbour / harbor");
    assert_eq!(
        result["presentation"]["matched_surfaces"],
        json!(["harbour", "harbor"])
    );
    assert_eq!(result["matches"].as_array().unwrap().len(), 1);
    assert_eq!(result["matches"][0]["dialect"], "uk");
    assert_eq!(result["matches"][0]["spelling"], "harbour");
    assert_eq!(result["matches"][0]["form_type"], "base");
    assert_eq!(
        result["matches"][0]["pos_id"],
        switching.v3_snapshot["forms"]["pos"][0]["pos_id"]
    );
    assert_eq!(
        result["matches"][0]["form_id"],
        switching.v3_snapshot["forms"]["pos"][0]["forms"][0]["id"]
    );
    assert_eq!(
        result["matches"][0]["variant_id"],
        switching.v3_snapshot["forms"]["pos"][0]["forms"][0]["regional_variants"]["uk"]["id"]
    );
    assert_eq!(
        result["senses"],
        json!([{"sense_id": switching.sense_id, "gloss": "新释义"}])
    );

    let (status, exact_us) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=harbor&kind=word&match_mode=exact&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exact_us}");
    assert_eq!(exact_us["total"], 1, "{exact_us}");
    assert_eq!(exact_us["results"][0]["matches"][0]["dialect"], "us");
    assert_eq!(exact_us["results"][0]["matches"][0]["spelling"], "harbor");

    let (status, excluded_exact) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=harbour&kind=word&match_mode=contains&exclude_exact=true&page_size=20"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{excluded_exact}");
    assert!(
        excluded_exact["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|candidate| candidate["entry_id"] != switching.entry_id.to_string()),
        "the exact V3 entry must be excluded: {excluded_exact}"
    );

    let (status, first_page) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=harb&kind=word&match_mode=contains&page_size=1"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first_page}");
    assert_eq!(first_page["total"], 2, "{first_page}");
    let cursor = first_page["next_cursor"].as_str().unwrap();
    let (status, flag_changed_cursor) = call(
        &read_disabled,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=harb&kind=word&match_mode=contains&page_size=1&cursor={cursor}"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{flag_changed_cursor}");
    assert_eq!(flag_changed_cursor["field"], "cursor");
    let (status, second_page) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=harb&kind=word&match_mode=contains&page_size=1&cursor={cursor}"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second_page}");
    assert_eq!(second_page["total"], 2);
    assert!(second_page["next_cursor"].is_null());
    let mut schemas = [
        first_page["results"][0]["schema_version"].as_u64().unwrap(),
        second_page["results"][0]["schema_version"]
            .as_u64()
            .unwrap(),
    ];
    schemas.sort_unstable();
    assert_eq!(schemas, [2, 3]);

    activate_related_publication(
        &state,
        &bearer,
        switching.entry_id,
        switching.v2_publication_id,
    )
    .await;
    let (status, restored_v2) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/entries/related-search?q=legacyharbour&kind=word&match_mode=exact&page_size=20"
        ),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored_v2}");
    assert_eq!(restored_v2["total"], 1);
    assert!(
        restored_v2["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|candidate| candidate["schema_version"] == 2),
        "{restored_v2}"
    );
    let (status, no_stale_v3) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/related-search?q=harbour&kind=word&match_mode=exact&page_size=20"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{no_stale_v3}");
    assert_eq!(no_stale_v3["total"], 0, "{no_stale_v3}");
}

#[sqlx::test]
async fn migrated_target_relations_and_contexts_follow_v2_then_v3_current_publication(
    pool: PgPool,
) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("test Redis connection should succeed");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let target = seed_migrated_target(&pool, admin_id, "legacyharbour").await;

    let v2_source = create_ready_v2_source(
        &state,
        &pool,
        &bearer,
        &format!("v2source{}", admin_id.simple()),
    )
    .await;
    let v2_source_id = Uuid::parse_str(v2_source["word"]["id"].as_str().unwrap()).unwrap();
    let v2_content = v2_reference_meanings(&v2_source, &target);
    let (status, v2_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v2_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": v2_source["word"]["revision"],
            "intent": "complete",
            "content": v2_content
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v2_saved}");
    assert_relation_snapshot(&v2_saved, "legacyharbour", "旧释义");

    let v3_source = create_v3_source_forms(
        &state,
        &pool,
        &bearer,
        &format!("vthree{}", admin_id.simple()),
    )
    .await;
    let v3_source_id = Uuid::parse_str(v3_source["word"]["id"].as_str().unwrap()).unwrap();
    let v3_pos_id = Uuid::parse_str(
        v3_source["word"]["forms"]["pos"][0]["pos_id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let v3_content =
        v3_reference_meanings(v3_source_id, v3_pos_id, target.entry_id, target.sense_id);
    let (status, v3_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v3_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": v3_source["word"]["revision"],
            "intent": "complete",
            "content": v3_content
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v3_saved}");
    assert_relation_snapshot(&v3_saved, "legacyharbour", "旧释义");

    let mut activation_body = json!({
        "schema_version": 3,
        "base_revision": 7,
        "base_lifecycle_revision": 1
    });
    let activation_path = format!(
        "{ROOT}/entries/{}/publications/{}/activate",
        target.entry_id, target.v3_publication_id
    );
    let activation_key = Uuid::now_v7();
    let (mut status, mut activated) = call(
        &state,
        Method::POST,
        &activation_path,
        &bearer,
        Some(activation_key),
        Some(activation_body.clone()),
    )
    .await;
    if status == StatusCode::CONFLICT
        && activated["code"] == "surface_match_acknowledgement_required"
    {
        activation_body["confirmed_surface_match_token"] =
            activated["meta"]["surface_match_page"]["surface_confirmation_token"].clone();
        (status, activated) = call(
            &state,
            Method::POST,
            &activation_path,
            &bearer,
            Some(activation_key),
            Some(activation_body),
        )
        .await;
    }
    assert_eq!(status, StatusCode::OK, "{activated}");
    assert_eq!(
        activated["word"]["presentation"]["label"],
        "harbour / harbor"
    );

    let v2_witness = source_witness(&pool, v2_source_id).await;
    let v3_witness = source_witness(&pool, v3_source_id).await;
    let (status, v2_stale) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{v2_source_id}/validate"),
        &bearer,
        None,
        Some(json!({"base_revision": v2_saved["word"]["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v2_stale}");
    assert!(has_issue(&v2_stale, "relation_target_stale"), "{v2_stale}");
    let (status, v3_stale) = call(
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
    assert_eq!(status, StatusCode::OK, "{v3_stale}");
    assert!(has_issue(&v3_stale, "relation_target_stale"), "{v3_stale}");
    assert_eq!(source_witness(&pool, v2_source_id).await, v2_witness);
    assert_eq!(source_witness(&pool, v3_source_id).await, v3_witness);

    let (status, v2_refreshed) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v2_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "base_revision": v2_saved["word"]["revision"],
            "intent": "complete",
            "content": v2_content
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v2_refreshed}");
    assert_relation_snapshot(&v2_refreshed, "harbour / harbor", "新释义");
    let (status, v3_refreshed) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v3_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": v3_saved["word"]["revision"],
            "intent": "complete",
            "content": v3_content
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v3_refreshed}");
    assert_relation_snapshot(&v3_refreshed, "harbour / harbor", "新释义");

    let stable_witness = source_witness(&pool, v3_source_id).await;
    sqlx::query("UPDATE lexicon.entries SET archived_at = now() WHERE id = $1")
        .bind(target.entry_id)
        .execute(&pool)
        .await
        .unwrap();
    let (status, archived) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v3_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": v3_refreshed["word"]["revision"],
            "intent": "complete",
            "content": v3_content
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{archived}");
    assert!(
        has_issue(&archived, "relation_target_unavailable"),
        "{archived}"
    );
    assert!(
        has_issue(&archived, "sentence_context_target_unavailable"),
        "{archived}"
    );
    assert_eq!(source_witness(&pool, v3_source_id).await, stable_witness);
    sqlx::query("UPDATE lexicon.entries SET archived_at = NULL WHERE id = $1")
        .bind(target.entry_id)
        .execute(&pool)
        .await
        .unwrap();

    let missing_sense = Uuid::now_v7();
    let mut missing_content = v3_content.clone();
    missing_content["pos"][0]["senses"][0]["relations"][0]["target_sense_id"] =
        json!(missing_sense);
    missing_content["pos"][0]["senses"][0]["sentences"][0]["links"][1]["sense_id"] =
        json!(missing_sense);
    let (status, missing) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v3_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": v3_refreshed["word"]["revision"],
            "intent": "complete",
            "content": missing_content
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{missing}");
    assert!(
        has_issue(&missing, "relation_target_unavailable"),
        "{missing}"
    );
    assert!(
        has_issue(&missing, "sentence_context_target_unavailable"),
        "{missing}"
    );
    assert_eq!(source_witness(&pool, v3_source_id).await, stable_witness);

    sqlx::query(
        r#"
        UPDATE lexicon.entry_publications
        SET snapshot = jsonb_set(snapshot, '{schema_version}', '99'::jsonb)
        WHERE id = $1
        "#,
    )
    .bind(target.v3_publication_id)
    .execute(&pool)
    .await
    .unwrap();
    let (status, unsupported) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v3_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": v3_refreshed["word"]["revision"],
            "intent": "complete",
            "content": v3_content
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{unsupported}");
    assert_eq!(unsupported["code"], "unsupported_schema_version");
    assert_eq!(source_witness(&pool, v3_source_id).await, stable_witness);
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot = $2 WHERE id = $1")
        .bind(target.v3_publication_id)
        .bind(target.v3_snapshot)
        .execute(&pool)
        .await
        .unwrap();
}

#[sqlx::test]
async fn sentence_associations_resolve_v3_targets_and_edit_v3_sources(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("test Redis connection should succeed");
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let target = seed_migrated_target(&pool, admin_id, "legacyharbour").await;
    let activated =
        activate_related_publication(&state, &bearer, target.entry_id, target.v3_publication_id)
            .await;
    assert_eq!(activated["word"]["schema_version"], 3);
    let target_form_id = Uuid::parse_str(
        target.v3_snapshot["forms"]["pos"][0]["forms"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    sqlx::query(
        "INSERT INTO lexicon.nodes (id, entry_id, node_type) VALUES ($1, $2, 'concrete_form') ON CONFLICT DO NOTHING",
    )
    .bind(target_form_id)
    .bind(target.entry_id)
    .execute(&pool)
    .await
    .unwrap();

    let source = create_ready_v2_source(
        &state,
        &pool,
        &bearer,
        &format!("v2sentence{}", admin_id.simple()),
    )
    .await;
    let source = save_v2_example_sentence(&state, &bearer, &source, "The harbour is calm.").await;
    let published = publish_v2_confirming(&state, &bearer, &source).await;
    assert_eq!(published["word"]["schema_version"], 2);
    let automatic =
        &published["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["associations"];
    assert_eq!(automatic.as_array().unwrap().len(), 1, "{published}");
    assert_eq!(automatic[0]["target_word_id"], target.entry_id.to_string());
    assert_eq!(automatic[0]["target_sense_id"], target.sense_id.to_string());
    assert_eq!(
        automatic[0]["target_form_slot_id"],
        target_form_id.to_string()
    );
    assert_eq!(automatic[0]["target_headword"], "harbour / harbor");
    assert_eq!(automatic[0]["target_gloss"], "新释义");
    assert_eq!(automatic[0]["resolved_form_type"], "base");

    let read_disabled = state
        .clone()
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags {
            read: false,
            ..SmartLexiconV3Flags::all_enabled()
        });
    let rollback_source = create_ready_v2_source(
        &state,
        &pool,
        &bearer,
        &format!("v2rollback{}", admin_id.simple()),
    )
    .await;
    let rollback_source =
        save_v2_example_sentence(&state, &bearer, &rollback_source, "The harbour is calm.").await;
    let rollback_published = publish_v2_confirming(&read_disabled, &bearer, &rollback_source).await;
    assert_eq!(
        rollback_published["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["associations"],
        json!([]),
        "V3-disabled V2 publish must not consume form_variant: {rollback_published}"
    );

    let v3_source = create_v3_source_forms(
        &state,
        &pool,
        &bearer,
        &format!("v3sentence{}", admin_id.simple()),
    )
    .await;
    let v3_source_id = Uuid::parse_str(v3_source["word"]["id"].as_str().unwrap()).unwrap();
    let v3_pos_id = Uuid::parse_str(
        v3_source["word"]["forms"]["pos"][0]["pos_id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let sentence_text = "The harbour is calm.";
    let mut meanings =
        v3_reference_meanings(v3_source_id, v3_pos_id, target.entry_id, target.sense_id);
    meanings["pos"][0]["senses"][0]["sentences"][0]["en_text"]["common"]["value"] =
        rich_text(sentence_text);
    let (status, v3_saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{v3_source_id}/steps/meanings"),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": v3_source["word"]["revision"],
            "intent": "complete",
            "content": meanings
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v3_saved}");
    let sentence_id = Uuid::parse_str(
        v3_saved["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.sentence_association_scans (
            sentence_id, entry_id, source_dialect, text_hash, resolver_version, scanned_at
        ) VALUES ($1, $2, 'common', $3, 1, now())
        "#,
    )
    .bind(sentence_id)
    .bind(v3_source_id)
    .bind(Sha256::digest(sentence_text.as_bytes()).to_vec())
    .execute(&pool)
    .await
    .unwrap();

    let association_id = Uuid::now_v7();
    let idempotency_key = Uuid::now_v7();
    let replace_body = json!({
        "base_revision": v3_saved["word"]["revision"],
        "base_lifecycle_revision": v3_saved["word"]["lifecycle_revision"],
        "associations": [{
            "id": association_id,
            "source_dialect": "common",
            "source_range": {"start": 4, "end": 11, "surface": "harbour"},
            "target_word_id": target.entry_id,
            "target_sense_id": target.sense_id
        }]
    });
    let replace_path =
        format!("{ROOT}/entries/{v3_source_id}/sentences/{sentence_id}/associations");
    let (status, edited) = call(
        &state,
        Method::PUT,
        &replace_path,
        &bearer,
        Some(idempotency_key),
        Some(replace_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["word"]["schema_version"], 3);
    let manual =
        &edited["word"]["meanings"]["pos"][0]["senses"][0]["sentences"][0]["associations"][0];
    assert_eq!(manual["id"], association_id.to_string());
    assert_eq!(manual["target_form_slot_id"], target_form_id.to_string());
    assert_eq!(manual["target_headword"], "harbour / harbor");
    assert_eq!(manual["target_gloss"], "新释义");

    let (status, replayed) = call(
        &state,
        Method::PUT,
        &replace_path,
        &bearer,
        Some(idempotency_key),
        Some(replace_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(replayed, edited);

    let (status, blocked_replay) = call(
        &read_disabled,
        Method::PUT,
        &replace_path,
        &bearer,
        Some(idempotency_key),
        Some(replace_body),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{blocked_replay}");
    assert_eq!(
        blocked_replay["code"],
        "smart_lexicon_v3_storage_unavailable"
    );
}
