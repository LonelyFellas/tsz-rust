//! Surface projection/query database contract tests (B2).

use std::time::Duration;

use sqlx::{PgPool, postgres::PgPoolOptions};
use tsz_rust::lexicon::{
    model::SurfaceLookupKey, repository::LexiconRepository, surface_policy::SurfacePolicyStore,
};
use uuid::Uuid;

/// 屏障解除后剩下的活儿只有「拿连接 + 抢咨询锁 + 一次 Redis CAS」，正常是毫秒级。
/// 给足余量只为在真死锁时仍能有界失败，不是对时延下断言——CI 上几百个连库测试并行
/// 抢同一个 Postgres/Redis 时，卡到秒级属于负载抖动，不该报成 bug。
const BARRIER_RELEASE_TIMEOUT: Duration = Duration::from_secs(30);

/// 阻塞到本库里至少有 `expected` 个未授予的 advisory lock 等待者。
///
/// 用来替代「sleep 一小会儿再假定它已经排队」：`transition` 的两条路径进锁时机不
/// 对称——enable 先抢锁，disable 先写一次 Redis 再抢锁——固定等待猜不准，排队顺序
/// 会随负载颠倒。而顺序是有意义的：Postgres 按等待队列的到达顺序授予咨询锁，所以
/// 「确认前一个真的排进队列了，再放下一个」就是本文件里 enable 必然先于 disable 被
/// 授予的全部依据。
///
/// 两个前提：`#[sqlx::test]` 给每个测试独立的库，因此本库里任何未授予的 advisory
/// lock 都属于当前测试（不必再按锁键过滤）；调用方连同被观测的两个任务和外层事务
/// 一共占 4 条连接，而 sqlx 给测试池的上限是 5，再多一个并发持连接方就会把本函数
/// 饿死在等连接上。
async fn await_advisory_lock_waiters(pool: &PgPool, expected: i64) {
    let deadline = tokio::time::Instant::now() + BARRIER_RELEASE_TIMEOUT;
    loop {
        let waiting: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM pg_locks
            WHERE locktype = 'advisory'
              AND NOT granted
              AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
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
            "等待 {expected} 个 advisory lock 排队者超时，当前 {waiting}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[sqlx::test]
async fn context_lock_waits_out_a_transaction_that_is_about_to_release(pool: PgPool) {
    // sqlx 的 Transaction::drop 只把 ROLLBACK 入队，等那条连接下次被异步使用才发出。
    // 所以一个刚失败返回的请求，它的 advisory xact lock 会短暂地继续挂着；紧接着的
    // 下一个请求从池里拿到别的连接，如果这里用 try-lock 就会凭空拿到 409。
    // 这个测试把「持锁方即将释放」这一幕固定下来：等待方必须等到它，而不是立刻失败。
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_entry(&pool, admin_id, "word", "lockwait", "lockwait").await;

    let mut holder = pool.begin().await.unwrap();
    LexiconRepository::lock_surface_contexts(&mut holder, &[entry_id])
        .await
        .expect("持锁方应当直接拿到锁");

    let waiter_pool = pool.clone();
    let waiter = tokio::spawn(async move {
        let mut waiter = waiter_pool.begin().await.unwrap();
        let outcome = LexiconRepository::lock_surface_contexts(&mut waiter, &[entry_id]).await;
        waiter.rollback().await.unwrap();
        outcome
    });

    // 确认等待方真的排进了队列——它没有立刻失败，这正是修复前后的分水岭。
    await_advisory_lock_waiters(&pool, 1).await;
    holder.rollback().await.unwrap();

    waiter
        .await
        .unwrap()
        .expect("持锁方释放后，等待方必须拿到锁而不是报 SurfaceContextBusy");
}

#[sqlx::test]
async fn context_lock_still_fails_fast_against_a_writer_that_keeps_holding(pool: PgPool) {
    // 有界等待不能退化成无限等待：真正的并发写者仍然要在上限内被判定为占用，
    // 否则连接池会被堵住。
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_entry(&pool, admin_id, "word", "lockbusy", "lockbusy").await;

    let mut holder = pool.begin().await.unwrap();
    LexiconRepository::lock_surface_contexts(&mut holder, &[entry_id])
        .await
        .unwrap();

    let mut waiter = pool.begin().await.unwrap();
    let outcome = LexiconRepository::lock_surface_contexts(&mut waiter, &[entry_id]).await;
    assert!(
        matches!(
            outcome,
            Err(tsz_rust::lexicon::repository::LexiconRepositoryError::SurfaceContextBusy)
        ),
        "持锁方一直不放时必须有界失败，实际：{outcome:?}"
    );
    waiter.rollback().await.unwrap();
    holder.rollback().await.unwrap();
}

#[sqlx::test]
async fn context_lock_reports_a_deadlock_as_busy_rather_than_an_internal_error(pool: PgPool) {
    // save_meanings 在同一事务里先锁自身、再锁关联词目标，所以两个互相引用的词条
    // 同时保存会按相反顺序抢同一对锁。try-lock 时代第二次直接返回 false；换成阻塞
    // 锁之后这里会变成真的 ABBA 死锁，必须仍然收敛成可重试的占用信号，而不是 500。
    let admin_id = insert_admin(&pool).await;
    let first = insert_entry(&pool, admin_id, "word", "deadlockone", "deadlockone").await;
    let second = insert_entry(&pool, admin_id, "word", "deadlocktwo", "deadlocktwo").await;

    let mut left = pool.begin().await.unwrap();
    LexiconRepository::lock_surface_contexts(&mut left, &[first])
        .await
        .unwrap();
    let mut right = pool.begin().await.unwrap();
    LexiconRepository::lock_surface_contexts(&mut right, &[second])
        .await
        .unwrap();

    // 两边交叉抢对方已持有的锁。
    let left_task = tokio::spawn(async move {
        let outcome = LexiconRepository::lock_surface_contexts(&mut left, &[second]).await;
        drop(left);
        outcome
    });
    let right_task = tokio::spawn(async move {
        let outcome = LexiconRepository::lock_surface_contexts(&mut right, &[first]).await;
        drop(right);
        outcome
    });

    let outcomes = [left_task.await.unwrap(), right_task.await.unwrap()];
    assert!(
        outcomes.iter().any(|outcome| matches!(
            outcome,
            Err(tsz_rust::lexicon::repository::LexiconRepositoryError::SurfaceContextBusy)
        )),
        "至少一方必须拿到可重试的 SurfaceContextBusy，实际：{outcomes:?}"
    );
    assert!(
        !outcomes.iter().any(|outcome| matches!(
            outcome,
            Err(tsz_rust::lexicon::repository::LexiconRepositoryError::Database(_))
        )),
        "死锁与超时都不得以内部数据库错误冒出去：{outcomes:?}"
    );
}

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
    tokio::time::timeout(BARRIER_RELEASE_TIMEOUT, barrier)
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
async fn policy_enable_waits_for_cutover_barrier_before_redis_cas(pool: PgPool) {
    let redis_url = std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .unwrap_or_else(|_| "redis://127.0.0.1:6379/0".to_owned());
    let redis = tsz_rust::platform::connect_redis(&redis_url)
        .await
        .expect("测试 Redis 连接池应能创建");
    let prefix = format!("test:surface-policy:{}:", Uuid::now_v7());
    let policy = SurfacePolicyStore::with_prefix_for_test(redis.clone(), prefix.clone());
    assert!(!policy.exact_headword_creation().await.unwrap().enabled);

    let mut cutover = pool.begin().await.unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('lexicon.surface-policy-writer', 0))",
    )
    .execute(&mut *cutover)
    .await
    .unwrap();

    let enable_pool = pool.clone();
    let enable_policy = policy.clone();
    let mut enable = Box::pin(async move {
        enable_policy
            .transition_exact_headword_creation(&enable_pool, true)
            .await
            .unwrap()
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut enable)
            .await
            .is_err(),
        "policy enable must wait while cutover holds the exclusive barrier"
    );
    assert!(
        !policy.exact_headword_creation().await.unwrap().enabled,
        "Redis CAS must not become visible before cutover releases its barrier"
    );

    cutover.commit().await.unwrap();
    let enabled = tokio::time::timeout(BARRIER_RELEASE_TIMEOUT, enable)
        .await
        .expect("policy enable should finish after cutover releases its barrier");
    assert!(enabled.enabled);

    let mut connection = redis.get().await.unwrap();
    deadpool_redis::redis::cmd("DEL")
        .arg(format!("{prefix}allow_new_exact_headword_entries"))
        .query_async::<()>(&mut connection)
        .await
        .unwrap();
}

#[sqlx::test]
async fn policy_disable_reasserts_false_after_an_inflight_enable(pool: PgPool) {
    let redis_url = std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .unwrap_or_else(|_| "redis://127.0.0.1:6379/0".to_owned());
    let redis = tsz_rust::platform::connect_redis(&redis_url)
        .await
        .expect("测试 Redis 连接池应能创建");
    let prefix = format!("test:surface-policy:{}:", Uuid::now_v7());
    let policy = SurfacePolicyStore::with_prefix_for_test(redis.clone(), prefix.clone());
    assert!(!policy.exact_headword_creation().await.unwrap().enabled);

    let mut outer_cutover = pool.begin().await.unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('lexicon.surface-policy-writer', 0))",
    )
    .execute(&mut *outer_cutover)
    .await
    .unwrap();

    // 两个转换都用独立任务驱动：屏障释放后谁先被授予锁都能自己跑完并释放。
    // 顺序靠 pg_locks 观测确认，不靠 sleep 猜。
    let enable_pool = pool.clone();
    let enable_policy = policy.clone();
    let enable = tokio::spawn(async move {
        enable_policy
            .transition_exact_headword_creation(&enable_pool, true)
            .await
            .unwrap()
    });
    await_advisory_lock_waiters(&pool, 1).await;
    assert!(
        !enable.is_finished(),
        "policy enable must wait while the outer cutover holds the exclusive barrier"
    );

    let disable_pool = pool.clone();
    let disable_policy = policy.clone();
    let disable = tokio::spawn(async move {
        disable_policy
            .transition_exact_headword_creation(&disable_pool, false)
            .await
            .unwrap()
    });
    await_advisory_lock_waiters(&pool, 2).await;
    assert!(
        !disable.is_finished(),
        "policy disable must queue behind the in-flight enable"
    );

    outer_cutover.commit().await.unwrap();
    let enabled = tokio::time::timeout(BARRIER_RELEASE_TIMEOUT, enable)
        .await
        .expect("queued enable should finish after the outer cutover")
        .unwrap();
    assert!(enabled.enabled);
    let disabled = tokio::time::timeout(BARRIER_RELEASE_TIMEOUT, disable)
        .await
        .expect("disable should obtain exclusive barrier after enable")
        .unwrap();
    assert!(!disabled.enabled);
    assert!(disabled.epoch > enabled.epoch);
    assert!(!policy.exact_headword_creation().await.unwrap().enabled);

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
        BARRIER_RELEASE_TIMEOUT,
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
