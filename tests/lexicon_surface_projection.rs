//! Surface projection/query database contract tests (B2).

use std::time::Duration;

use sqlx::{PgPool, postgres::PgPoolOptions};
use tsz_rust::lexicon::{
    model::SurfaceLookupKey, repository::LexiconRepository, surface_policy::SurfacePolicyStore,
};
use uuid::Uuid;

async fn insert_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', 'surface projection test')",
    )
    .bind(id)
    .bind(format!("surface-projection-{}", id.simple()))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn insert_entry(
    pool: &PgPool,
    admin_id: Uuid,
    kind: &str,
    headword: &str,
    normalized_headword: &str,
) -> Uuid {
    let entry_id = Uuid::now_v7();
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

    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_headwords (
            id, entry_id, dialect, headword, normalized_headword,
            normalization_version, origin
        ) VALUES ($1, $2, 'common', $3, $4, 1, 'manual')
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(entry_id)
    .bind(headword)
    .bind(normalized_headword)
    .execute(pool)
    .await
    .unwrap();
    entry_id
}

async fn insert_publication(pool: &PgPool, admin_id: Uuid, entry_id: Uuid, number: i32) -> Uuid {
    let publication_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publications (
            id, entry_id, publication_number, source_revision,
            content_schema_version, snapshot, snapshot_hash, published_by_admin_id
        ) VALUES ($1, $2, $3, $3, 2, '{}', $4, $5)
        "#,
    )
    .bind(publication_id)
    .bind(entry_id)
    .bind(number)
    .bind(publication_id.as_bytes().to_vec())
    .bind(admin_id)
    .execute(pool)
    .await
    .unwrap();
    publication_id
}

struct SurfaceSource<'a> {
    entry_id: Uuid,
    source_id: &'a str,
    source_kind: &'a str,
    source_node_id: Option<Uuid>,
    entry_kind: &'a str,
    dialect: &'a str,
    dialect_scope: &'a str,
    surface: &'a str,
    normalized_surface: &'a str,
    source_revision: i64,
    is_deleted: bool,
    content_scope: &'a str,
    publication_id: Option<Uuid>,
    pos_id: Option<Uuid>,
    pos: Option<&'a str>,
    form_type: Option<&'a str>,
}

async fn insert_surface(pool: &PgPool, source: SurfaceSource<'_>) {
    sqlx::query(
        r#"
        INSERT INTO lexicon.surface_sources (
            entry_id, source_id, source_kind, source_node_id, language,
            entry_kind, dialect, dialect_scope, surface, normalized_surface,
            normalization_version, source_revision, is_deleted, content_scope,
            publication_id, pos_id, pos, form_type
        ) VALUES (
            $1, $2, $3, $4, 'en', $5, $6, $7, $8, $9,
            1, $10, $11, $12, $13, $14, $15, $16
        )
        "#,
    )
    .bind(source.entry_id)
    .bind(source.source_id)
    .bind(source.source_kind)
    .bind(source.source_node_id)
    .bind(source.entry_kind)
    .bind(source.dialect)
    .bind(source.dialect_scope)
    .bind(source.surface)
    .bind(source.normalized_surface)
    .bind(source.source_revision)
    .bind(source.is_deleted)
    .bind(source.content_scope)
    .bind(source.publication_id)
    .bind(source.pos_id)
    .bind(source.pos)
    .bind(source.form_type)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn detection_consumption_is_globally_unique_across_actors(pool: PgPool) {
    let first_actor = insert_admin(&pool).await;
    let second_actor = insert_admin(&pool).await;
    let first_entry = insert_entry(&pool, first_actor, "word", "alpha", "alpha").await;
    let second_entry = insert_entry(&pool, second_actor, "word", "beta", "beta").await;
    let detection_id = Uuid::now_v7();

    sqlx::query(
        "INSERT INTO lexicon.consumed_detections (actor_id, detection_id, entry_id) VALUES ($1, $2, $3)",
    )
    .bind(first_actor)
    .bind(detection_id)
    .bind(first_entry)
    .execute(&pool)
    .await
    .unwrap();

    let error = sqlx::query(
        "INSERT INTO lexicon.consumed_detections (actor_id, detection_id, entry_id) VALUES ($1, $2, $3)",
    )
    .bind(second_actor)
    .bind(detection_id)
    .bind(second_entry)
    .execute(&pool)
    .await
    .unwrap_err();
    let database = error.as_database_error().expect("database error");
    assert_eq!(database.code().as_deref(), Some("23505"));
    assert_eq!(
        database.constraint(),
        Some("consumed_detections_detection_id_key")
    );
}

fn headword_source<'a>(
    entry_id: Uuid,
    source_id: &'a str,
    entry_kind: &'a str,
    dialect: &'a str,
    dialect_scope: &'a str,
    surface: &'a str,
    normalized_surface: &'a str,
) -> SurfaceSource<'a> {
    SurfaceSource {
        entry_id,
        source_id,
        source_kind: "headword",
        source_node_id: None,
        entry_kind,
        dialect,
        dialect_scope,
        surface,
        normalized_surface,
        source_revision: 1,
        is_deleted: false,
        content_scope: "draft",
        publication_id: None,
        pos_id: None,
        pos: None,
        form_type: None,
    }
}

fn lookup(scope: &str, normalized_surface: &str) -> SurfaceLookupKey {
    SurfaceLookupKey {
        dialect_scope: scope.to_owned(),
        normalized_surface: normalized_surface.to_owned(),
    }
}

#[sqlx::test]
async fn lookup_is_non_unique_and_kind_independent(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let word_id = insert_entry(&pool, admin_id, "word", "workspace", "workspace").await;
    let phrase_id = insert_entry(&pool, admin_id, "phrase", "workspace", "workspace").await;

    insert_surface(
        &pool,
        headword_source(
            word_id,
            &format!("{word_id}:headword:common"),
            "word",
            "common",
            "uk",
            "workspace",
            "workspace",
        ),
    )
    .await;
    insert_surface(
        &pool,
        headword_source(
            phrase_id,
            &format!("{phrase_id}:headword:common"),
            "phrase",
            "common",
            "uk",
            "workspace",
            "workspace",
        ),
    )
    .await;

    let repository = LexiconRepository::new(pool.clone());
    let matches = repository
        .surface_sources("en", &[lookup("uk", "workspace")], None)
        .await
        .unwrap();

    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].entry_kind, "phrase");
    assert_eq!(matches[1].entry_kind, "word");
    assert_eq!(matches[0].entry_headword, "workspace");
    assert_eq!(matches[1].entry_headword, "workspace");

    let legacy_unique_still_present: bool = sqlx::query_scalar(
        "SELECT to_regclass('lexicon.lexicon_entry_headword_keys_unique_idx') IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        legacy_unique_still_present,
        "B2 不得提前执行 B4 UNIQUE cutover"
    );
}

#[sqlx::test]
async fn common_expands_to_both_scopes_and_only_explicit_forms_match(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_entry(&pool, admin_id, "word", "workspace", "workspace").await;
    let variant_id = Uuid::now_v7();
    let pos_id = Uuid::now_v7();
    let source_id = variant_id.to_string();
    let headword_source_id = format!("{entry_id}:headword:common");

    for scope in ["uk", "us"] {
        insert_surface(
            &pool,
            headword_source(
                entry_id,
                &headword_source_id,
                "word",
                "common",
                scope,
                "workspace",
                "workspace",
            ),
        )
        .await;
        insert_surface(
            &pool,
            SurfaceSource {
                entry_id,
                source_id: &source_id,
                source_kind: "form",
                source_node_id: Some(variant_id),
                entry_kind: "word",
                dialect: "common",
                dialect_scope: scope,
                surface: "workspaces",
                normalized_surface: "workspaces",
                source_revision: 2,
                is_deleted: false,
                content_scope: "draft",
                publication_id: None,
                pos_id: Some(pos_id),
                pos: Some("noun"),
                form_type: Some("plural"),
            },
        )
        .await;
    }

    let repository = LexiconRepository::new(pool);
    let matches = repository
        .surface_sources(
            "en",
            &[lookup("uk", "workspaces"), lookup("us", "workspaces")],
            None,
        )
        .await
        .unwrap();
    assert_eq!(matches.len(), 2);
    assert!(matches.iter().all(|item| item.source_id == source_id));
    assert!(
        matches
            .iter()
            .all(|item| item.form_type.as_deref() == Some("plural"))
    );
    assert_eq!(matches[0].matched_dialect_scope, "uk");
    assert_eq!(matches[1].matched_dialect_scope, "us");

    let inferred = repository
        .surface_sources("en", &[lookup("uk", "workspaced")], None)
        .await
        .unwrap();
    assert!(inferred.is_empty(), "查询不得实现英语词形推断");
}

#[sqlx::test]
async fn lookup_filters_tombstones_and_non_current_publications_but_keeps_archived(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_entry(&pool, admin_id, "word", "workspace", "workspace").await;
    let old_publication = insert_publication(&pool, admin_id, entry_id, 1).await;
    let current_publication = insert_publication(&pool, admin_id, entry_id, 2).await;
    sqlx::query(
        "UPDATE lexicon.entries SET current_publication_id = $2, archived_at = now() WHERE id = $1",
    )
    .bind(entry_id)
    .bind(current_publication)
    .execute(&pool)
    .await
    .unwrap();

    let draft_source_id = format!("{entry_id}:headword:common");
    let mut draft = headword_source(
        entry_id,
        &draft_source_id,
        "word",
        "common",
        "uk",
        "workspace",
        "workspace",
    );
    draft.source_revision = 3;
    insert_surface(&pool, draft).await;

    for (source_id, publication_id) in [
        (format!("{entry_id}:publication:old"), old_publication),
        (
            format!("{entry_id}:publication:current"),
            current_publication,
        ),
    ] {
        let mut publication = headword_source(
            entry_id,
            &source_id,
            "word",
            "common",
            "uk",
            "workspace",
            "workspace",
        );
        publication.content_scope = "current_publication";
        publication.publication_id = Some(publication_id);
        publication.source_revision = if publication_id == current_publication {
            2
        } else {
            1
        };
        insert_surface(&pool, publication).await;
    }

    let tombstone_source_id = format!("{entry_id}:deleted");
    let mut tombstone = headword_source(
        entry_id,
        &tombstone_source_id,
        "word",
        "common",
        "uk",
        "workspace",
        "workspace",
    );
    tombstone.is_deleted = true;
    tombstone.source_revision = 4;
    insert_surface(&pool, tombstone).await;

    let repository = LexiconRepository::new(pool);
    let matches = repository
        .surface_sources("en", &[lookup("uk", "workspace")], None)
        .await
        .unwrap();

    assert_eq!(
        matches.len(),
        2,
        "只保留 current draft 与 current publication"
    );
    assert!(
        matches
            .iter()
            .all(|item| item.lifecycle_status == "archived")
    );
    assert!(matches.iter().any(|item| item.content_scope == "draft"));
    assert!(matches.iter().any(|item| {
        item.content_scope == "current_publication"
            && item.publication_id == Some(current_publication)
    }));
    assert!(
        !matches
            .iter()
            .any(|item| item.publication_id == Some(old_publication))
    );

    let excluded = repository
        .surface_sources("en", &[lookup("uk", "workspace")], Some(entry_id))
        .await
        .unwrap();
    assert!(excluded.is_empty());
}

#[sqlx::test]
async fn policy_disable_barrier_waits_for_inflight_surface_writer(pool: PgPool) {
    let redis_url = std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .unwrap_or_else(|_| "redis://127.0.0.1:6379/0".to_owned());
    let redis = tsz_rust::platform::connect_redis(&redis_url)
        .await
        .expect("测试 Redis 连接池应能创建");
    let prefix = format!("test:surface-policy:{}:", Uuid::now_v7());
    let policy = SurfacePolicyStore::with_prefix_for_test(redis.clone(), prefix.clone());
    let enabled = policy
        .transition_exact_headword_creation(&pool, true)
        .await
        .unwrap();
    assert!(enabled.enabled);

    let mut writer = pool.begin().await.unwrap();
    LexiconRepository::lock_surface_policy_writer(&mut writer)
        .await
        .unwrap();

    let barrier_pool = pool.clone();
    let barrier_policy = policy.clone();
    let mut barrier = Box::pin(async move {
        barrier_policy
            .transition_exact_headword_creation(&barrier_pool, false)
            .await
            .unwrap();
    });

    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut barrier)
            .await
            .is_err(),
        "exclusive disable barrier must wait while a create holds the shared writer barrier"
    );
    writer.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), barrier)
        .await
        .expect("disable barrier should pass after the writer commits");

    let disabled = policy.exact_headword_creation().await.unwrap();
    assert!(!disabled.enabled);
    assert_eq!(disabled.epoch, enabled.epoch + 1);
    let mut connection = redis.get().await.unwrap();
    deadpool_redis::redis::cmd("DEL")
        .arg(format!("{prefix}allow_new_exact_headword_entries"))
        .query_async::<()>(&mut connection)
        .await
        .unwrap();
}

#[sqlx::test]
async fn lock_held_surface_requery_uses_the_same_connection(pool: PgPool) {
    let connect_options = pool.connect_options().as_ref().clone();
    let single_connection_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options)
        .await
        .unwrap();
    let mut transaction = single_connection_pool.begin().await.unwrap();
    LexiconRepository::lock_surface_policy_writer(&mut transaction)
        .await
        .unwrap();

    let rows = tokio::time::timeout(
        Duration::from_secs(2),
        LexiconRepository::surface_sources_in_transaction(
            &mut transaction,
            "en",
            &[lookup("uk", "does-not-exist")],
            None,
        ),
    )
    .await
    .expect("transaction query must not wait for a second pool connection")
    .unwrap();
    assert!(rows.is_empty());
    transaction.commit().await.unwrap();
}
