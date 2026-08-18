use std::{sync::Arc, time::Duration};

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
    lexicon::content_completion::{
        ContentCompletionRepository, OpenAiContentGenerator, OpenAiLexiconConfig,
    },
    state::AppState,
};

async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("completion-{}", id.simple()),
            display_name: "内容生成测试管理员".to_owned(),
            password_hash: "hashed-password".to_owned(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin 应成功");
    id
}

async fn seed_entry(pool: &PgPool, admin_id: Uuid) -> (Uuid, Uuid) {
    let entry_id = Uuid::now_v7();
    let pos_id = Uuid::now_v7();
    sqlx::query(
        r#"INSERT INTO lexicon.entries (
               id, language, kind, revision, headword_mode, detection_snapshot,
               created_by_admin_id, updated_by_admin_id
           ) VALUES ($1, 'en', 'word', 1, 'unified', '{}'::jsonb, $2, $2)"#,
    )
    .bind(entry_id)
    .bind(admin_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO lexicon.entry_headwords (
               id, entry_id, dialect, headword, normalized_headword,
               normalization_version, origin
           ) VALUES ($1, $2, 'common', 'bank', 'bank', 1, 'manual')"#,
    )
    .bind(Uuid::now_v7())
    .bind(entry_id)
    .execute(pool)
    .await
    .unwrap();
    let forms = json!({
        "pos": [{
            "pos_id": pos_id,
            "pos": "noun",
            "dialect_rules": {"spelling_mode": "unified", "phonetic_mode": "unified"},
            "base_form": {"id": Uuid::now_v7(), "form_type": "base", "variants": []},
            "form_groups": []
        }]
    });
    sqlx::query(
        r#"INSERT INTO lexicon.entry_editor_projection
           (entry_id, forms, meanings, rebuilt_revision)
           VALUES ($1, $2, '{"sense_groups":[],"pos":[]}'::jsonb, 1)"#,
    )
    .bind(entry_id)
    .bind(forms)
    .execute(pool)
    .await
    .unwrap();
    (entry_id, pos_id)
}

async fn seed_dictionary_content(pool: &PgPool) {
    let dataset_id: i64 = sqlx::query_scalar(
        r#"INSERT INTO dictionary.datasets (
               version, source_name, source_version, rules_version,
               terms_sha256, regions_sha256, status
           ) VALUES (
               'content-test-v1', 'Kaikki English Wiktionary', 'enwiktionary-test',
               'test-rules', 'terms', 'regions', 'active'
           ) RETURNING id"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO dictionary.terms (
               dataset_id, normalized_term, term, kind, pos, status,
               sense_count, filtered_cold_sense_count, region_family
           ) VALUES ($1, 'bank', 'bank', 'word', ARRAY['noun'], 'accepted', 2, 0, 'common_unmarked')"#,
    )
    .bind(dataset_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO dictionary.entry_contents (
               dataset_id, source_key, normalized_term, pos, senses, source_locator
           ) VALUES ($1, 'kaikki:bank:noun:test', 'bank', 'noun', $2, $3)"#,
    )
    .bind(dataset_id)
    .bind(json!([{
        "glosses": ["A financial institution"],
        "examples": [{"text": "I deposited the cheque at the bank."}]
    }]))
    .bind("https://kaikki.org/dictionary/English/meaning/b/ba/bank.jsonl")
    .execute(pool)
    .await
    .unwrap();
}

fn token(state: &AppState, admin_id: Uuid) -> String {
    state
        .admin_token_manager
        .generate(admin_id, AdminRole::Admin.as_str())
        .unwrap()
}

fn configured_state(pool: PgPool) -> AppState {
    let mut state = AppState::for_test(pool);
    state.lexicon_content_generator = Some(Arc::new(
        OpenAiContentGenerator::new(OpenAiLexiconConfig {
            api_key: "test-only".to_owned(),
            model: "test-model".to_owned(),
            base_url: "http://127.0.0.1:1/v1".to_owned(),
            timeout: Duration::from_millis(10),
        })
        .unwrap(),
    ));
    state
}

async fn call(
    state: &AppState,
    method: Method,
    uri: &str,
    bearer: &str,
    idempotency_key: Option<Uuid>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    if let Some(key) = idempotency_key {
        request = request.header("Idempotency-Key", key.to_string());
    }
    let body = if let Some(value) = body {
        request = request.header(header::CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&value).unwrap())
    } else {
        Body::empty()
    };
    let response = tsz_rust::router(state.clone())
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap();
    (status, body)
}

fn create_input(revision: i64) -> Value {
    json!({
        "base_revision": revision,
        "scope": ["grammar_structures", "meanings", "examples"],
        "fill_policy": "missing_only"
    })
}

#[sqlx::test]
async fn create_is_idempotent_and_job_is_readable(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let (entry_id, pos_id) = seed_entry(&pool, admin_id).await;
    seed_dictionary_content(&pool).await;
    let state = configured_state(pool);
    let bearer = token(&state, admin_id);
    let key = Uuid::now_v7();
    let uri = format!("/api/v1/admin/lexicon/entries/{entry_id}/content-completion-jobs");

    let (status, first) = call(
        &state,
        Method::POST,
        &uri,
        &bearer,
        Some(key),
        Some(create_input(1)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(first["job"]["status"], "pending");
    assert_eq!(first["job"]["partitions"][0]["pos_id"], pos_id.to_string());
    let source_snapshot: Value = sqlx::query_scalar(
        "SELECT source_snapshot FROM lexicon.content_completion_jobs WHERE id=$1",
    )
    .bind(Uuid::parse_str(first["job"]["id"].as_str().unwrap()).unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        source_snapshot["dictionary_provider"],
        "Kaikki English Wiktionary"
    );
    assert_eq!(
        source_snapshot["dictionary_evidence_by_pos"]["noun"][0]["source_key"],
        "kaikki:bank:noun:test"
    );

    let (status, replay) = call(
        &state,
        Method::POST,
        &uri,
        &bearer,
        Some(key),
        Some(create_input(1)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(replay["job"]["id"], first["job"]["id"]);

    let (other_entry_id, _) = seed_entry(&state.pool, admin_id).await;
    let (status, problem) = call(
        &state,
        Method::POST,
        &format!("/api/v1/admin/lexicon/entries/{other_entry_id}/content-completion-jobs"),
        &bearer,
        Some(key),
        Some(create_input(1)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"], "idempotency_conflict");

    let job_id = first["job"]["id"].as_str().unwrap();
    let (status, fetched) = call(
        &state,
        Method::GET,
        &format!("{uri}/{job_id}"),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["job"]["id"], job_id);

    sqlx::query(
        "UPDATE lexicon.content_completion_partitions SET status='missing', error_code='source_not_found' WHERE job_id=$1 AND pos_id=$2",
    )
    .bind(Uuid::parse_str(job_id).unwrap())
    .bind(pos_id)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE lexicon.content_completion_jobs SET status='failed' WHERE id=$1")
        .bind(Uuid::parse_str(job_id).unwrap())
        .execute(&state.pool)
        .await
        .unwrap();
    let retry_key = Uuid::now_v7();
    let retry_uri = format!("{uri}/{job_id}/retries");
    let (status, retried) = call(
        &state,
        Method::POST,
        &retry_uri,
        &bearer,
        Some(retry_key),
        Some(json!({"pos_ids": [pos_id]})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(retried["job"]["status"], "pending");
    assert_eq!(retried["job"]["partitions"][0]["status"], "pending");
    let (status, replayed_retry) = call(
        &state,
        Method::POST,
        &retry_uri,
        &bearer,
        Some(retry_key),
        Some(json!({"pos_ids": [pos_id]})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(replayed_retry["job"]["id"], job_id);

    let (status, problem) = call(
        &state,
        Method::POST,
        &uri,
        &bearer,
        Some(key),
        Some(create_input(2)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"], "idempotency_conflict");
}

#[sqlx::test]
async fn concurrent_create_requests_are_idempotent_and_conflict_safely(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let (entry_id, _) = seed_entry(&pool, admin_id).await;
    seed_dictionary_content(&pool).await;
    let state = configured_state(pool);
    let bearer = token(&state, admin_id);
    let key = Uuid::now_v7();
    let uri = format!("/api/v1/admin/lexicon/entries/{entry_id}/content-completion-jobs");

    let (first, second) = tokio::join!(
        call(
            &state,
            Method::POST,
            &uri,
            &bearer,
            Some(key),
            Some(create_input(1))
        ),
        call(
            &state,
            Method::POST,
            &uri,
            &bearer,
            Some(key),
            Some(create_input(1))
        )
    );
    assert_eq!(first.0, StatusCode::ACCEPTED);
    assert_eq!(second.0, StatusCode::ACCEPTED);
    assert_eq!(first.1["job"]["id"], second.1["job"]["id"]);

    let (other_entry_id, _) = seed_entry(&state.pool, admin_id).await;
    let conflict_key = Uuid::now_v7();
    let other_uri =
        format!("/api/v1/admin/lexicon/entries/{other_entry_id}/content-completion-jobs");
    let (left, right) = tokio::join!(
        call(
            &state,
            Method::POST,
            &uri,
            &bearer,
            Some(conflict_key),
            Some(create_input(1))
        ),
        call(
            &state,
            Method::POST,
            &other_uri,
            &bearer,
            Some(conflict_key),
            Some(create_input(1))
        )
    );
    let statuses = [left.0, right.0];
    assert!(statuses.contains(&StatusCode::ACCEPTED));
    assert!(statuses.contains(&StatusCode::CONFLICT));
    let conflict = if left.0 == StatusCode::CONFLICT {
        left.1
    } else {
        right.1
    };
    assert_eq!(conflict["code"], "idempotency_conflict");
}

#[sqlx::test]
async fn create_fails_closed_without_provider_and_rejects_stale_revision(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let (entry_id, _) = seed_entry(&pool, admin_id).await;
    let unconfigured = AppState::for_test(pool.clone());
    let bearer = token(&unconfigured, admin_id);
    let uri = format!("/api/v1/admin/lexicon/entries/{entry_id}/content-completion-jobs");

    let (status, problem) = call(
        &unconfigured,
        Method::POST,
        &uri,
        &bearer,
        Some(Uuid::now_v7()),
        Some(create_input(1)),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(problem["code"], "service_unavailable");

    let configured = configured_state(pool);
    let (status, problem) = call(
        &configured,
        Method::POST,
        &uri,
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": 1,
            "scope": ["meanings"],
            "fill_policy": "missing_only"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["code"], "validation_failed");

    let (status, problem) = call(
        &configured,
        Method::POST,
        &uri,
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "base_revision": 1,
            "scope": ["grammar_structures", "meanings", "examples", "examples"],
            "fill_policy": "missing_only"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["code"], "validation_failed");

    let (status, problem) = call(
        &configured,
        Method::POST,
        &uri,
        &bearer,
        Some(Uuid::now_v7()),
        Some(create_input(2)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"], "revision_conflict");
    assert_eq!(problem["meta"]["current_revision"], 1);
}

#[sqlx::test]
async fn stale_worker_attempt_cannot_overwrite_reclaimed_partition(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let (entry_id, pos_id) = seed_entry(&pool, admin_id).await;
    seed_dictionary_content(&pool).await;
    let state = configured_state(pool);
    let bearer = token(&state, admin_id);
    let uri = format!("/api/v1/admin/lexicon/entries/{entry_id}/content-completion-jobs");
    let (status, created) = call(
        &state,
        Method::POST,
        &uri,
        &bearer,
        Some(Uuid::now_v7()),
        Some(create_input(1)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job_id = Uuid::parse_str(created["job"]["id"].as_str().unwrap()).unwrap();
    let repository = ContentCompletionRepository::new(state.pool.clone());
    let first = repository.claim().await.unwrap().unwrap();
    assert_eq!(first.attempt, 1);
    sqlx::query(
        "UPDATE lexicon.content_completion_partitions SET lease_expires_at=now()-interval '1 second' WHERE job_id=$1 AND pos_id=$2",
    )
    .bind(job_id)
    .bind(pos_id)
    .execute(&state.pool)
    .await
    .unwrap();
    let second = repository.claim().await.unwrap().unwrap();
    assert_eq!(second.attempt, 2);

    repository
        .fail_partition(job_id, pos_id, first.attempt, "stale", "stale worker")
        .await
        .unwrap();
    let row: (String, i32) = sqlx::query_as(
        "SELECT status,attempt FROM lexicon.content_completion_partitions WHERE job_id=$1 AND pos_id=$2",
    )
    .bind(job_id)
    .bind(pos_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(row, ("running".to_owned(), 2));

    repository
        .fail_partition(job_id, pos_id, second.attempt, "current", "current worker")
        .await
        .unwrap();
    let status: String = sqlx::query_scalar(
        "SELECT status FROM lexicon.content_completion_partitions WHERE job_id=$1 AND pos_id=$2",
    )
    .bind(job_id)
    .bind(pos_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(status, "failed");
}
