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
