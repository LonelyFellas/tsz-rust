//! Surface projection/query database contract tests (B2).

use std::time::Duration;

use sqlx::PgPool;
use tsz_rust::lexicon::{repository::LexiconRepository, surface_policy::SurfacePolicyStore};
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
    let entry_id = insert_entry(&pool, admin_id, "word").await;

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
    let entry_id = insert_entry(&pool, admin_id, "word").await;

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
    let first = insert_entry(&pool, admin_id, "word").await;
    let second = insert_entry(&pool, admin_id, "word").await;

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

async fn insert_entry(pool: &PgPool, admin_id: Uuid, kind: &str) -> Uuid {
    let entry_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entries (
            id, content_schema_version, language, kind, revision, detection_snapshot,
            created_by_admin_id, updated_by_admin_id
        ) VALUES ($1, 3, 'en', $2, 1, '{}', $3, $3)
        "#,
    )
    .bind(entry_id)
    .bind(kind)
    .bind(admin_id)
    .execute(pool)
    .await
    .unwrap();
    entry_id
}

#[sqlx::test]
async fn detection_consumption_is_globally_unique_across_actors(pool: PgPool) {
    let first_actor = insert_admin(&pool).await;
    let second_actor = insert_admin(&pool).await;
    let first_entry = insert_entry(&pool, first_actor, "word").await;
    let second_entry = insert_entry(&pool, second_actor, "word").await;
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
