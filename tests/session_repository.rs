//! `RefreshTokenRepository` 的行为测试（真库，`#[sqlx::test]`）。
//!
//! 每个测试拿独立临时库、自动跑 `migrations/`、结束回滚。验的是**仓储层的 SQL 行为**：
//! 插入/按哈希查回、轮换/吊销的时间戳落位、`revoke_all_for_user` 的范围与计数。
//!
//! ⚠️ 边界划分（重要）：repository **只搬 SQL**——
//!   - 不做哈希：`token_hash` 存的就是入参原值（明文→哈希是 service 的活），这里用普通字符串当
//!     "假哈希"，只验「存进去 = 查出来」，不验加密。
//!   - 不判过期：`expires_at` 过去的行照样能插、能查回（过期判定在 service）。本文件专门有一条钉住它。

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use tsz_rust::session::model::NewRefreshToken;
use tsz_rust::session::repository::RefreshTokenRepository;

/// 造一个 FK 依赖的用户，返回其 id。phone 用随机 id 串保证唯一（表对 phone 有部分唯一索引）。
async fn seed_user(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO users (id, phone, password_hash, display_name) VALUES ($1, $2, $3, $4)",
        id,
        id.to_string(),
        "hashed-pw",
        "测试用户",
    )
    .execute(pool)
    .await
    .expect("seed user 应成功");
    id
}

/// 默认 30 天后过期的 NewRefreshToken。
fn new_token(user_id: Uuid, token_hash: &str) -> NewRefreshToken {
    new_token_expiring(user_id, token_hash, Utc::now() + Duration::days(30))
}

fn new_token_expiring(user_id: Uuid, token_hash: &str, expires_at: DateTime<Utc>) -> NewRefreshToken {
    NewRefreshToken {
        id: Uuid::now_v7(),
        user_id,
        token_hash: token_hash.to_owned(),
        expires_at,
    }
}

// ————————————————————— insert / find_by_hash —————————————————————

/// 插入后能按哈希原样查回，字段无损，新行 revoked/rotated 都为 NULL。
#[sqlx::test]
async fn insert_then_find_by_hash_roundtrips(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);

    let inserted = repo
        .insert(new_token(user_id, "hash-abc"))
        .await
        .expect("insert 应成功");

    // insert 的返回值：DB 回填了 created_at、id 就是传入的。
    assert_eq!(inserted.user_id, user_id);
    assert_eq!(inserted.token_hash, "hash-abc");
    assert!(inserted.revoked_at.is_none(), "新行不应已吊销");
    assert!(inserted.rotated_at.is_none(), "新行不应已轮换");

    // 按哈希查回，和 insert 返回的是同一行。
    let found = repo
        .find_by_hash("hash-abc")
        .await
        .expect("查询不应报错")
        .expect("应命中该哈希");
    assert_eq!(found.id, inserted.id);
    assert_eq!(found.user_id, user_id);
    assert_eq!(
        found.token_hash, "hash-abc",
        "repository 应原样存哈希、不做任何变换"
    );
}

/// 查不到的哈希 → Ok(None)，不是错误（未命中是正常结果，不是异常）。
#[sqlx::test]
async fn find_by_hash_miss_returns_none(pool: PgPool) {
    let repo = RefreshTokenRepository::new(pool);
    let got = repo
        .find_by_hash("no-such-hash")
        .await
        .expect("未命中应是 Ok(None) 而非 Err");
    assert!(got.is_none(), "不存在的哈希应返回 None");
}

/// 过期时刻在过去的行，repository 照样能插、能查回——过期判定不归它管。
#[sqlx::test]
async fn expired_row_is_still_insertable_and_readable(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);

    let past = Utc::now() - Duration::days(1);
    repo.insert(new_token_expiring(user_id, "hash-expired", past))
        .await
        .expect("过期行也应能插入");

    let found = repo
        .find_by_hash("hash-expired")
        .await
        .expect("查询不应报错")
        .expect("过期行也应能查回");
    assert!(
        found.expires_at < Utc::now(),
        "查回的行 expires_at 应在过去（repository 不按过期过滤）"
    );
}

// ————————————————————— mark_rotated / revoke —————————————————————

/// mark_rotated 只盖 rotated_at，不碰 revoked_at。
#[sqlx::test]
async fn mark_rotated_sets_only_rotated_at(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);
    let row = repo.insert(new_token(user_id, "hash-rot")).await.unwrap();

    repo.mark_rotated(row.id).await.expect("mark_rotated 应成功");

    let found = repo.find_by_hash("hash-rot").await.unwrap().unwrap();
    assert!(found.rotated_at.is_some(), "rotated_at 应被置上");
    assert!(found.revoked_at.is_none(), "mark_rotated 不应动 revoked_at");
}

/// revoke 只盖 revoked_at，不碰 rotated_at。
#[sqlx::test]
async fn revoke_sets_only_revoked_at(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);
    let row = repo.insert(new_token(user_id, "hash-rev")).await.unwrap();

    repo.revoke(row.id).await.expect("revoke 应成功");

    let found = repo.find_by_hash("hash-rev").await.unwrap().unwrap();
    assert!(found.revoked_at.is_some(), "revoked_at 应被置上");
    assert!(found.rotated_at.is_none(), "revoke 不应动 rotated_at");
}

// ————————————————————— revoke_all_for_user —————————————————————

/// 吊销该用户所有【未吊销】的 token，返回准确计数，且不波及别的用户。
#[sqlx::test]
async fn revoke_all_for_user_revokes_only_that_users_active_tokens(pool: PgPool) {
    let alice = seed_user(&pool).await;
    let bob = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);

    // Alice 两枚，Bob 一枚（token_hash 唯一，取不同值）。
    repo.insert(new_token(alice, "a1")).await.unwrap();
    repo.insert(new_token(alice, "a2")).await.unwrap();
    repo.insert(new_token(bob, "b1")).await.unwrap();

    let n = repo
        .revoke_all_for_user(alice)
        .await
        .expect("revoke_all 应成功");
    assert_eq!(n, 2, "应恰好吊销 Alice 的 2 枚");

    // Alice 两枚都被吊销。
    for h in ["a1", "a2"] {
        let row = repo.find_by_hash(h).await.unwrap().unwrap();
        assert!(row.revoked_at.is_some(), "Alice 的 {h} 应已吊销");
    }
    // Bob 的不受影响。
    let bob_row = repo.find_by_hash("b1").await.unwrap().unwrap();
    assert!(bob_row.revoked_at.is_none(), "Bob 的 token 不应被波及");
}

/// 幂等：对已全部吊销的用户再调一次，返回 0（`AND revoked_at IS NULL` 守卫生效），
/// 且不会覆盖原有的吊销时刻。
#[sqlx::test]
async fn revoke_all_for_user_is_idempotent(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);
    repo.insert(new_token(user_id, "h1")).await.unwrap();

    let first = repo.revoke_all_for_user(user_id).await.unwrap();
    assert_eq!(first, 1, "首次应吊销 1 枚");

    // 记下首次吊销时刻。
    let after_first = repo
        .find_by_hash("h1")
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .expect("应已吊销");

    let second = repo.revoke_all_for_user(user_id).await.unwrap();
    assert_eq!(second, 0, "已全吊销后再调应返回 0（守卫生效）");

    // 第二次不应覆盖 revoked_at。
    let after_second = repo
        .find_by_hash("h1")
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .expect("仍应是吊销态");
    assert_eq!(after_first, after_second, "重复调用不应刷新原吊销时刻");
}

// ————————————————————— consume（CAS 轮换消费）—————————————————————
// consume = 一条 UPDATE...WHERE 未轮换/未吊销/未过期...RETURNING user_id 的原子 CAS。
// 抢到返回 Some(user_id) 并盖上 rotated_at；任何失效态或未命中都返回 None（对内不可区分，
// 对外由 handler 统一收敛成 401）。以下钉住这套状态机的每条边。

/// happy：活跃 token 被 consume → 返回属主 user_id，且该行 rotated_at 被置上、revoked_at 不动。
#[sqlx::test]
async fn consume_active_token_returns_owner_and_marks_rotated(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);
    repo.insert(new_token(user_id, "hash-live")).await.unwrap();

    let got = repo.consume("hash-live").await.expect("consume 不应报错");
    assert_eq!(got, Some(user_id), "抢到应返回属主 user_id");

    let row = repo.find_by_hash("hash-live").await.unwrap().unwrap();
    assert!(row.rotated_at.is_some(), "consume 应盖上 rotated_at");
    assert!(row.revoked_at.is_none(), "consume 不应动 revoked_at");
}

/// 复用即拒（CAS 的命根子）：同一枚 consume 两次，第二次必 None。
#[sqlx::test]
async fn consume_twice_second_time_misses(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);
    repo.insert(new_token(user_id, "hash-once")).await.unwrap();

    assert_eq!(
        repo.consume("hash-once").await.unwrap(),
        Some(user_id),
        "首次应抢到"
    );
    assert_eq!(
        repo.consume("hash-once").await.unwrap(),
        None,
        "已轮换的再 consume 必落空"
    );
}

/// 已吊销的 token 不能被 consume。
#[sqlx::test]
async fn consume_revoked_token_misses(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);
    let row = repo
        .insert(new_token(user_id, "hash-revoked"))
        .await
        .unwrap();
    repo.revoke(row.id).await.unwrap();

    assert_eq!(
        repo.consume("hash-revoked").await.unwrap(),
        None,
        "已吊销的不该能 consume"
    );
}

/// 已过期的 token 不能被 consume，且不应被误标 rotated（WHERE expires_at > NOW() 守住）。
#[sqlx::test]
async fn consume_expired_token_misses_and_leaves_it_untouched(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);
    let past = Utc::now() - Duration::days(1);
    repo.insert(new_token_expiring(user_id, "hash-expired", past))
        .await
        .unwrap();

    assert_eq!(
        repo.consume("hash-expired").await.unwrap(),
        None,
        "过期的不该能 consume"
    );

    let row = repo.find_by_hash("hash-expired").await.unwrap().unwrap();
    assert!(row.rotated_at.is_none(), "过期行不应被误盖 rotated_at");
}

/// 不存在的哈希 → None（未命中是正常结果，不是错误）。
#[sqlx::test]
async fn consume_unknown_hash_misses(pool: PgPool) {
    let repo = RefreshTokenRepository::new(pool);
    assert_eq!(repo.consume("no-such-hash").await.unwrap(), None);
}

/// 并发抢同一枚：两个 consume 同时打，恰好一个拿到 Some、另一个 None。
/// 这是 CAS 相对 find-then-mark 的核心优势——数据库行级原子性天然序列化，无需应用层锁。
#[sqlx::test]
async fn concurrent_consume_only_one_wins(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);
    repo.insert(new_token(user_id, "hash-race")).await.unwrap();

    let (a, b) = tokio::join!(repo.consume("hash-race"), repo.consume("hash-race"));
    let (a, b) = (a.expect("不应报错"), b.expect("不应报错"));

    assert!(
        a.is_some() ^ b.is_some(),
        "并发下应恰好一个抢到（a={a:?}, b={b:?}）"
    );
    assert_eq!(a.or(b), Some(user_id), "抢到的那个应返回属主 user_id");
}

// ————————————————————— revoke_by_hash（logout）—————————————————————

/// 按哈希吊销：命中未吊销行 → 返回 1、盖 revoked_at、不动 rotated_at；行仍可查回。
#[sqlx::test]
async fn revoke_by_hash_revokes_matching_row(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);
    repo.insert(new_token(user_id, "hash-logout"))
        .await
        .unwrap();

    let n = repo
        .revoke_by_hash("hash-logout")
        .await
        .expect("revoke_by_hash 不应报错");
    assert_eq!(n, 1, "应吊销 1 行");

    let row = repo.find_by_hash("hash-logout").await.unwrap().unwrap();
    assert!(row.revoked_at.is_some(), "应盖上 revoked_at");
    assert!(row.rotated_at.is_none(), "不应动 rotated_at");
}

/// 幂等：再吊销一次返回 0（AND revoked_at IS NULL 守卫），不刷新原吊销时刻。
#[sqlx::test]
async fn revoke_by_hash_is_idempotent(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);
    repo.insert(new_token(user_id, "hash-idem")).await.unwrap();

    assert_eq!(repo.revoke_by_hash("hash-idem").await.unwrap(), 1);
    let first = repo
        .find_by_hash("hash-idem")
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .unwrap();

    assert_eq!(
        repo.revoke_by_hash("hash-idem").await.unwrap(),
        0,
        "已吊销再调应返回 0"
    );
    let second = repo
        .find_by_hash("hash-idem")
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .unwrap();
    assert_eq!(first, second, "重复吊销不应刷新时刻");
}

/// 吊销后不能再被 consume（logout 让 token 立即失效）。
#[sqlx::test]
async fn revoked_by_hash_token_cannot_be_consumed(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool);
    repo.insert(new_token(user_id, "hash-dead")).await.unwrap();
    repo.revoke_by_hash("hash-dead").await.unwrap();

    assert_eq!(
        repo.consume("hash-dead").await.unwrap(),
        None,
        "吊销后 consume 应落空"
    );
}

/// 不存在的哈希 → 0（幂等 logout 不因此报错）。
#[sqlx::test]
async fn revoke_by_hash_unknown_returns_zero(pool: PgPool) {
    let repo = RefreshTokenRepository::new(pool);
    assert_eq!(repo.revoke_by_hash("no-such").await.unwrap(), 0);
}
