//! Smart Lexicon V3 lifecycle routing and atomicity contracts.

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    config::SmartLexiconV3Flags,
    lexicon::dto::SurfacePolicyNameV2,
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
            phone: format!("v3-lifecycle-{}", id.simple()),
            display_name: "V3 lifecycle tester".to_owned(),
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
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

async fn seed_v3_entry(pool: &PgPool, admin_id: Uuid) -> Uuid {
    let entry_id = Uuid::now_v7();
    let pos_id = Uuid::now_v7();
    let group_id = Uuid::now_v7();
    let membership_id = Uuid::now_v7();
    let form_id = Uuid::now_v7();
    let variant_id = Uuid::now_v7();
    let label = "native lifecycle";
    let surface = "native-lifecycle";
    let forms = json!({
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
                        "spelling": surface,
                        "origin": "manual",
                        "pronunciations": []
                    }
                }
            }],
            "form_groups": [{
                "id": group_id,
                "is_regular": true,
                "members": [{"id": membership_id, "form_id": form_id}]
            }]
        }]
    });
    sqlx::query(
        r#"
        INSERT INTO lexicon.entries (
            id, content_schema_version, language, kind, revision,
            detection_snapshot, created_by_admin_id, updated_by_admin_id
        ) VALUES ($1, 3, 'en', 'word', 1, '{}', $2, $2)
        "#,
    )
    .bind(entry_id)
    .bind(admin_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_editor_projection (
            entry_id, forms, meanings, rebuilt_revision
        ) VALUES ($1, $2, '{"sense_groups":[],"pos":[]}', 1)
        "#,
    )
    .bind(entry_id)
    .bind(forms)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_presentation_projection (
            entry_id, content_schema_version, source_revision,
            label, matched_surfaces, strategy_version
        ) VALUES ($1, 3, 1, $2, ARRAY[$3]::text[], 'lifecycle-test-v1')
        "#,
    )
    .bind(entry_id)
    .bind(label)
    .bind(surface)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_entry_state (entry_id, origin) VALUES ($1, 'native')
        "#,
    )
    .bind(entry_id)
    .execute(pool)
    .await
    .unwrap();
    for dialect_scope in ["uk", "us"] {
        sqlx::query(
            r#"
            INSERT INTO lexicon.surface_sources (
                entry_id, source_id, source_kind, source_node_id,
                language, entry_kind, dialect, dialect_scope,
                surface, normalized_surface, normalization_version,
                source_revision, is_deleted, content_scope,
                pos_id, pos, form_type, content_schema_version,
                form_id, variant_id, group_ids, projection_version
            ) VALUES (
                $1, $2, 'form_variant', $3,
                'en', 'word', 'common', $4,
                $5, $5, 1,
                1, FALSE, 'draft',
                $6, 'noun', 'base', 3,
                $7, $3, ARRAY[$8]::uuid[], 'lifecycle-test-v1'
            )
            "#,
        )
        .bind(entry_id)
        .bind(format!(
            "entry:{entry_id}:form:{form_id}:variant:{variant_id}"
        ))
        .bind(variant_id)
        .bind(dialect_scope)
        .bind(surface)
        .bind(pos_id)
        .bind(form_id)
        .bind(group_id)
        .execute(pool)
        .await
        .unwrap();
    }
    entry_id
}

async fn seed_v3_empty_skeleton(pool: &PgPool, admin_id: Uuid, surface: &str) -> Uuid {
    let entry_id = seed_v3_entry(pool, admin_id).await;
    let detection = json!({
        "schema_version": 3,
        "normalized_surface": surface,
        "request": {"language": "en", "kind": "word", "surface": surface}
    });
    sqlx::query(
        r#"
        UPDATE lexicon.entries
        SET detection_snapshot = $2
        WHERE id = $1
        "#,
    )
    .bind(entry_id)
    .bind(detection)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        UPDATE lexicon.v3_entry_state
        SET initial_headwords = $2,
            initial_headword_keys = $3
        WHERE entry_id = $1
        "#,
    )
    .bind(entry_id)
    .bind(json!({"mode": "unified", "common": surface}))
    .bind(vec![format!("uk:{surface}"), format!("us:{surface}")])
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        UPDATE lexicon.entry_editor_projection
        SET forms = '{"pos":[]}'::jsonb
        WHERE entry_id = $1
        "#,
    )
    .bind(entry_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        UPDATE lexicon.entry_presentation_projection
        SET label = $2,
            matched_surfaces = ARRAY[$2]::text[]
        WHERE entry_id = $1
        "#,
    )
    .bind(entry_id)
    .bind(surface)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE lexicon.surface_sources SET is_deleted = TRUE WHERE entry_id = $1")
        .bind(entry_id)
        .execute(pool)
        .await
        .unwrap();
    entry_id
}

async fn archive_v3_entry(state: &AppState, bearer: &str, entry_id: Uuid) -> Value {
    let (status, archived) = call(
        state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/archive"),
        bearer,
        Some(Uuid::now_v7()),
        Some(json!({"base_revision": 1, "base_lifecycle_revision": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    archived
}

async fn wait_for_lifecycle_row_lock_waiter(pool: &PgPool) {
    for _ in 0..100 {
        let blocked: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM pg_stat_activity
                WHERE datname = current_database()
                  AND wait_event_type = 'Lock'
                  AND query = 'SELECT id FROM lexicon.entries WHERE id = $1 FOR UPDATE'
            )
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if blocked {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("lifecycle request did not reach the authoritative entry row lock");
}

async fn wait_for_surface_context_lock_waiter(pool: &PgPool) {
    for _ in 0..100 {
        let blocked: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM pg_stat_activity
                WHERE datname = current_database()
                  AND wait_event_type = 'Lock'
                  AND wait_event = 'advisory'
            )
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if blocked {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("lifecycle request did not reach the surface-context lock");
}

#[sqlx::test]
async fn v3_single_lifecycle_delete_and_legacy_bridge_are_versioned(pool: PgPool) {
    let state = AppState::for_test_with_smart_lexicon_v3_flags(
        pool.clone(),
        SmartLexiconV3Flags::all_enabled(),
    );
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let native_id = seed_v3_entry(&pool, admin_id).await;

    let key = Uuid::now_v7();
    let archive_body = json!({"base_revision": 1, "base_lifecycle_revision": 1});
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{native_id}/archive"),
        &bearer,
        Some(key),
        Some(archive_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    assert_eq!(archived["word"]["schema_version"], 3);
    assert_eq!(archived["word"]["status"], "archived");
    assert_eq!(archived["word"]["revision"], 1);
    assert_eq!(archived["word"]["lifecycle_revision"], 2);
    assert_eq!(
        archived["word"]["presentation"]["label"],
        "native lifecycle"
    );

    let (status, replayed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{native_id}/archive"),
        &bearer,
        Some(key),
        Some(archive_body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(replayed, archived);

    let (status, restored) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{native_id}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({"base_revision": 1, "base_lifecycle_revision": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["word"]["schema_version"], 3);
    assert_eq!(restored["word"]["status"], "draft");
    assert_eq!(restored["word"]["lifecycle_revision"], 3);
    let preserved_projection: (i64, Option<i64>, i64) = sqlx::query_as(
        r#"
        SELECT
          count(*) FILTER (WHERE source.is_deleted = FALSE),
          max(source.source_revision),
          (SELECT presentation.source_revision
           FROM lexicon.entry_presentation_projection presentation
           WHERE presentation.entry_id = $1)
        FROM lexicon.surface_sources source
        WHERE source.entry_id = $1
        "#,
    )
    .bind(native_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        preserved_projection,
        (2, Some(1), 1),
        "archive/restore must preserve canonical V3 projection revisions"
    );

    let second_id = seed_v3_entry(&pool, admin_id).await;
    let (status, second_archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{second_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({"base_revision": 1, "base_lifecycle_revision": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second_archived}");
    assert_eq!(second_archived["word"]["schema_version"], 3);
    assert!(second_archived["word"].get("compatibility").is_none());

    let deleted_id = seed_v3_entry(&pool, admin_id).await;
    let (status, body) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{deleted_id}"),
        &bearer,
        None,
        Some(json!({"base_revision": 1, "base_lifecycle_revision": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.entries WHERE id = $1")
        .bind(deleted_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    let projection_rows: (i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
          (SELECT count(*) FROM lexicon.entry_presentation_projection WHERE entry_id = $1),
          (SELECT count(*) FROM lexicon.surface_sources
           WHERE entry_id = $1 AND is_deleted = FALSE),
          (SELECT count(*) FROM lexicon.surface_sources
           WHERE entry_id = $1 AND is_deleted = TRUE)
        "#,
    )
    .bind(deleted_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        projection_rows,
        (0, 0, 2),
        "V3 delete must remove presentation and retain only surface tombstones"
    );

    let published_id = seed_v3_entry(&pool, admin_id).await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publications (
            id, entry_id, publication_number, source_revision,
            content_schema_version, snapshot, snapshot_hash,
            published_by_admin_id, published_at
        ) VALUES ($1, $2, 1, 1, 3, '{"schema_version":3}', $3, $4, now())
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(published_id)
    .bind(Uuid::now_v7().as_bytes().to_vec())
    .bind(admin_id)
    .execute(&pool)
    .await
    .unwrap();
    let (status, problem) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/entries/{published_id}"),
        &bearer,
        None,
        Some(json!({"base_revision": 1, "base_lifecycle_revision": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(problem["code"], "entry_not_deletable");
    let still_present: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon.entries WHERE id = $1)")
            .bind(published_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(still_present);
}

#[sqlx::test]
async fn empty_v3_restore_uses_initial_headword_conflicts_for_single_batch_and_locks(pool: PgPool) {
    let state = AppState::for_test_with_smart_lexicon_v3_flags(
        pool.clone(),
        SmartLexiconV3Flags::all_enabled(),
    );
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);

    let surface = format!("restore-hidden-{}", admin_id.simple());
    let restoring = seed_v3_empty_skeleton(&pool, admin_id, &surface).await;
    archive_v3_entry(&state, &bearer, restoring).await;
    let _incumbent = seed_v3_empty_skeleton(&pool, admin_id, &surface).await;
    let restore_body = json!({"base_revision": 1, "base_lifecycle_revision": 2});
    let (status, duplicate) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{restoring}/restore"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(restore_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");
    assert_eq!(duplicate["code"], "duplicate_word");

    let mut key_lock = pool.begin().await.unwrap();
    sqlx::query(
        r#"
        WITH requested AS MATERIALIZED (
            SELECT language, dialect_scope, normalized_surface
            FROM unnest($1::text[], $2::text[], $3::text[])
                AS value(language, dialect_scope, normalized_surface)
            ORDER BY language, dialect_scope, normalized_surface
        )
        SELECT pg_advisory_xact_lock(hashtextextended(
            'lexicon.surface:' || requested.language || ':' ||
            requested.dialect_scope || ':' || requested.normalized_surface,
            0
        ))
        FROM requested
        "#,
    )
    .bind(vec!["en", "en"])
    .bind(vec!["uk", "us"])
    .bind(vec![surface.clone(), surface.clone()])
    .execute(&mut *key_lock)
    .await
    .unwrap();
    let concurrent_state = state.clone();
    let concurrent_bearer = bearer.clone();
    let concurrent_restore = tokio::spawn(async move {
        call(
            &concurrent_state,
            Method::POST,
            &format!("{ROOT}/entries/{restoring}/restore"),
            &concurrent_bearer,
            Some(Uuid::now_v7()),
            Some(restore_body),
        )
        .await
    });
    wait_for_surface_context_lock_waiter(&pool).await;
    key_lock.commit().await.unwrap();
    let (status, duplicate) = tokio::time::timeout(Duration::from_secs(5), concurrent_restore)
        .await
        .expect("restore should resume after the initial-headword key lock")
        .unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");
    assert_eq!(duplicate["code"], "duplicate_word");

    let batch_surface = format!("restore-hidden-batch-{}", admin_id.simple());
    let first = seed_v3_empty_skeleton(&pool, admin_id, &batch_surface).await;
    let second = seed_v3_empty_skeleton(&pool, admin_id, &batch_surface).await;
    archive_v3_entry(&state, &bearer, first).await;
    archive_v3_entry(&state, &bearer, second).await;
    let (status, batch_duplicate) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/restore-batch"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({
            "entries": [
                {"id": first, "base_revision": 1, "base_lifecycle_revision": 2},
                {"id": second, "base_revision": 1, "base_lifecycle_revision": 2}
            ]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{batch_duplicate}");
    assert_eq!(batch_duplicate["code"], "duplicate_word");
    let still_archived: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM lexicon.entries WHERE id = ANY($1) AND archived_at IS NOT NULL",
    )
    .bind(vec![first, second])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(still_archived, 2);
}

#[sqlx::test]
async fn v3_restore_requires_one_snapshot_then_audits_and_replays(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let target_id = seed_v3_entry(&pool, admin_id).await;
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({"base_revision": 1, "base_lifecycle_revision": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    let collision_id = seed_v3_entry(&pool, admin_id).await;

    let restore_body = json!({"base_revision": 1, "base_lifecycle_revision": 2});
    let key = Uuid::now_v7();
    let (status, required) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/restore"),
        &bearer,
        Some(key),
        Some(restore_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{required}");
    assert_eq!(required["code"], "surface_match_acknowledgement_required");
    assert_eq!(required["meta"]["surface_match_page"]["schema_version"], 3);
    assert!(
        required["meta"]["surface_match_page"]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| {
                item["match_kind"] == "form_variant_v3"
                    && item["match"]["source_schema_version"] == 3
                    && item["match"]["entry_id"] == collision_id.to_string()
            })),
        "{required}"
    );
    let token = required["meta"]["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let unchanged: (i64, bool) = sqlx::query_as(
        "SELECT lifecycle_revision, archived_at IS NOT NULL FROM lexicon.entries WHERE id = $1",
    )
    .bind(target_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unchanged, (2, true));

    let mut confirmed_body = restore_body;
    confirmed_body["confirmed_surface_match_token"] = json!(token);
    let (status, restored) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/restore"),
        &bearer,
        Some(key),
        Some(confirmed_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["word"]["schema_version"], 3);
    assert_eq!(restored["word"]["lifecycle_revision"], 3);
    let audit_count: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM audit.admin_actions
        WHERE resource_id = $1
          AND action = 'lexicon.surface_warning.acknowledge_command'
        "#,
    )
    .bind(target_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audit_count, 1);

    let (status, replayed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/restore"),
        &bearer,
        Some(key),
        Some(confirmed_body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(replayed, restored);
}

#[sqlx::test]
async fn v3_restore_stale_candidate_and_policy_epoch_fail_without_writes(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let target_id = seed_v3_entry(&pool, admin_id).await;
    let (status, archived) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/archive"),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({"base_revision": 1, "base_lifecycle_revision": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{archived}");
    seed_v3_entry(&pool, admin_id).await;
    let restore_body = json!({"base_revision": 1, "base_lifecycle_revision": 2});
    let key = Uuid::now_v7();
    let (status, required) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/restore"),
        &bearer,
        Some(key),
        Some(restore_body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{required}");
    let stale_token = required["meta"]["surface_match_page"]["surface_confirmation_token"]
        .as_str()
        .unwrap();

    seed_v3_entry(&pool, admin_id).await;
    let mut stale_body = restore_body.clone();
    stale_body["confirmed_surface_match_token"] = json!(stale_token);
    let (status, changed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/restore"),
        &bearer,
        Some(key),
        Some(stale_body),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{changed}");
    assert_eq!(changed["code"], "surface_matches_changed");
    assert_eq!(changed["meta"]["surface_match_page"]["schema_version"], 3);
    let replacement = changed["meta"]["surface_match_page"]["surface_confirmation_token"]
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
    let mut policy_body = restore_body;
    policy_body["confirmed_surface_match_token"] = json!(replacement);
    let (status, policy_changed) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{target_id}/restore"),
        &bearer,
        Some(key),
        Some(policy_body),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{policy_changed}");
    assert_eq!(policy_changed["code"], "surface_policy_changed");
    let unchanged: (i64, bool) = sqlx::query_as(
        "SELECT lifecycle_revision, archived_at IS NOT NULL FROM lexicon.entries WHERE id = $1",
    )
    .bind(target_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unchanged, (2, true));
}

#[sqlx::test]
async fn v3_lifecycle_locks_surface_context_before_the_entry_row(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let state = AppState::for_test_with_smart_lexicon_v3_flags(
        pool.clone(),
        SmartLexiconV3Flags::all_enabled(),
    );
    let bearer = bearer(&state, admin_id);
    let entry_id = seed_v3_entry(&pool, admin_id).await;

    let mut context_gate = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("lexicon.surface-context:{entry_id}"))
        .execute(&mut *context_gate)
        .await
        .unwrap();

    let request_state = state.clone();
    let request_bearer = bearer.clone();
    let request = tokio::spawn(async move {
        call(
            &request_state,
            Method::POST,
            &format!("{ROOT}/entries/{entry_id}/archive"),
            &request_bearer,
            Some(Uuid::now_v7()),
            Some(json!({"base_revision": 1, "base_lifecycle_revision": 1})),
        )
        .await
    });
    wait_for_surface_context_lock_waiter(&pool).await;

    sqlx::query("SET LOCAL lock_timeout = '1s'")
        .execute(&mut *context_gate)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM lexicon.entries WHERE id = $1 FOR UPDATE")
        .bind(entry_id)
        .execute(&mut *context_gate)
        .await
        .expect("lifecycle must not hold the entry row while waiting for its surface context");
    context_gate.commit().await.unwrap();

    let (status, archived) = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .expect("archive must finish without an advisory/row deadlock")
        .unwrap();
    assert_eq!(status, StatusCode::OK, "{archived}");
    assert_eq!(archived["word"]["status"], "archived");
}

#[sqlx::test]
async fn v3_lifecycle_reads_the_projection_committed_under_the_entry_lock(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let state = AppState::for_test_with_smart_lexicon_v3_flags(
        pool.clone(),
        SmartLexiconV3Flags::all_enabled(),
    );
    let bearer = bearer(&state, admin_id);
    let entry_id = seed_v3_entry(&pool, admin_id).await;

    let mut writer = pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE lexicon.entries SET revision = 2, updated_at = now() WHERE id = $1 AND revision = 1",
    )
    .bind(entry_id)
    .execute(&mut *writer)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE lexicon.entry_editor_projection SET rebuilt_revision = 2 WHERE entry_id = $1",
    )
    .bind(entry_id)
    .execute(&mut *writer)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE lexicon.entry_presentation_projection SET source_revision = 2, label = 'locked projection' WHERE entry_id = $1",
    )
    .bind(entry_id)
    .execute(&mut *writer)
    .await
    .unwrap();

    let request_state = state.clone();
    let request_bearer = bearer.clone();
    let request = tokio::spawn(async move {
        call(
            &request_state,
            Method::POST,
            &format!("{ROOT}/entries/{entry_id}/archive"),
            &request_bearer,
            Some(Uuid::now_v7()),
            Some(json!({"base_revision": 2, "base_lifecycle_revision": 1})),
        )
        .await
    });
    wait_for_lifecycle_row_lock_waiter(&pool).await;
    writer.commit().await.unwrap();

    let (status, archived) = request.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{archived}");
    assert_eq!(archived["word"]["revision"], 2);
    assert_eq!(archived["word"]["lifecycle_revision"], 2);
    assert_eq!(
        archived["word"]["presentation"]["label"],
        "locked projection"
    );
}
