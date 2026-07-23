//! `AdminRefreshTokenRepository` 的行为测试（真库，`#[sqlx::test]`）。
//!
//! web 侧 `RefreshTokenRepository`（tests/session_repository.rs）的平移，表换成
//! `admin_refresh_tokens`，方法集缩到 5 个（admin-design.md §5）：
//!   `insert / find_by_hash / consume_and_insert / revoke_by_hash / revoke_all_by_admin_id`
//! ——**没有**独立的 consume/mark_rotated/revoke：轮换消费只有原子版一条路，
//! 单行吊销只按哈希（logout 语义），不存在按 id 吊销的场景。
//!
//! **Q8 偏离（本文件的重点）**：`consume_and_insert(old_hash, new_hash, new_id)`
//! **不收 expires_at 参数**——新枚继承被消费旧枚的 `expires_at`（CTE 单条 SQL 里
//! `UPDATE...RETURNING admin_id, expires_at` 直接喂 INSERT，继承在 DB 层原子完成，
//! service 想传错都没有入口）。返回 `Option<(admin_id, expires_at)>`：
//! expires_at 即继承到的「本次登录绝对死线」，service 拿去组装响应/cookie。
//!
//! ⚠️ 边界划分同 web：repository 只搬 SQL——不做哈希（token_hash 原样存，测试用
//! 普通字符串当"假哈希"）、不判过期语义（过期行照样插/查，过期只在 consume 的
//! WHERE 里挡）。「严格单登录」（Q1 的 issue 前 revoke_all）是 service 编排，
//! repo 的 insert 不执法——本文件专门一条钉住这个边界。
//!
//! 契约（等 src/admin/session.rs 实现，admin/mod.rs 需 re-export）：
//!   `AdminRefreshToken { id, admin_id, token_hash, revoked_at, rotated_at, expires_at, created_at }`
//!   `NewAdminRefreshToken { id, admin_id, token_hash, expires_at }`
//!   `AdminRefreshTokenRepository::new(pool)`

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use tsz_rust::admin::{
    AdminRefreshTokenRepository, AdminRepository, AdminRole, NewAdmin, NewAdminRefreshToken,
};

/// 造一个 FK 依赖的管理员，返回其 id。phone 用 UUIDv7 串保证并行测试不撞唯一索引。
async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.to_string(),
            display_name: "测试管理员".to_owned(),
            password_hash: "hashed-pw".to_owned(),
            role: AdminRole::Admin,
            must_change_password: true,
        })
        .await
        .expect("seed admin 应成功");
    id
}

/// 默认 7 天后过期的 NewAdminRefreshToken（镜像 ADMIN_REFRESH_TTL_DAYS 默认值，仅测试便利）。
fn new_token(admin_id: Uuid, token_hash: &str) -> NewAdminRefreshToken {
    new_token_expiring(admin_id, token_hash, Utc::now() + Duration::days(7))
}

fn new_token_expiring(
    admin_id: Uuid,
    token_hash: &str,
    expires_at: DateTime<Utc>,
) -> NewAdminRefreshToken {
    NewAdminRefreshToken {
        id: Uuid::now_v7(),
        admin_id,
        token_hash: token_hash.to_owned(),
        expires_at,
    }
}

// ————————————————————— insert / find_by_hash —————————————————————

/// 插入后能按哈希原样查回，字段无损，新行 revoked/rotated 都为 NULL。
#[sqlx::test]
async fn insert_then_find_by_hash_roundtrips(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);

    let inserted = repo
        .insert(new_token(admin_id, "hash-abc"))
        .await
        .expect("insert 应成功");
    assert_eq!(inserted.admin_id, admin_id);
    assert_eq!(inserted.token_hash, "hash-abc");
    assert!(inserted.revoked_at.is_none(), "新行不应已吊销");
    assert!(inserted.rotated_at.is_none(), "新行不应已轮换");

    let found = repo
        .find_by_hash("hash-abc")
        .await
        .expect("查询不应报错")
        .expect("应命中该哈希");
    assert_eq!(found.id, inserted.id);
    assert_eq!(found.admin_id, admin_id);
    assert_eq!(
        found.token_hash, "hash-abc",
        "repository 应原样存哈希、不做任何变换"
    );
}

/// 查不到的哈希 → Ok(None)，不是错误。
#[sqlx::test]
async fn find_by_hash_miss_returns_none(pool: PgPool) {
    let repo = AdminRefreshTokenRepository::new(pool);
    let got = repo
        .find_by_hash("no-such-hash")
        .await
        .expect("未命中应是 Ok(None) 而非 Err");
    assert!(got.is_none());
}

/// 过期行照样能插、能查回——过期判定不归 repository 管。
#[sqlx::test]
async fn expired_row_is_still_insertable_and_readable(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);

    let past = Utc::now() - Duration::days(1);
    repo.insert(new_token_expiring(admin_id, "hash-expired", past))
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

/// 同一 admin 多枚并存：repo 的 insert **不执法单登录**——严格单登录（Q1 的
/// issue 前 revoke_all）是 service 编排的活，谁也别在 repo 层重复执法。
#[sqlx::test]
async fn insert_allows_multiple_live_tokens_per_admin(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);

    repo.insert(new_token(admin_id, "h1")).await.unwrap();
    repo.insert(new_token(admin_id, "h2")).await.unwrap();

    for h in ["h1", "h2"] {
        let row = repo.find_by_hash(h).await.unwrap().unwrap();
        assert!(
            row.revoked_at.is_none(),
            "repo 层 insert 不得顺手吊销旧枚（单登录归 service）：{h}"
        );
    }
}

// ——————————————— consume_and_insert（原子轮换，Q8：继承死线）———————————————
// 「消费旧枚 + 插入新枚」同生共死；新枚的 expires_at 不是参数，而是从旧枚继承——
// 轮换只换凭证不续命，expires_at 就是本次登录的绝对死线。

/// happy：旧枚标 rotated、新枚落库且活跃；返回 (属主, 继承的死线)。
/// 死线断言用 insert RETURNING 的 DB 回读值（微秒精度），不用 Rust 侧原值——
/// TIMESTAMPTZ 截断纳秒，拿内存值比对会假红。
#[sqlx::test]
async fn consume_and_insert_swaps_old_for_new_and_inherits_deadline(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);

    // 死线刻意取 3 天——不等于任何 ttl 默认值（7d/30d），实现若偷偷重算 now+ttl 立刻现形
    let old = repo
        .insert(new_token_expiring(
            admin_id,
            "old-hash",
            Utc::now() + Duration::days(3),
        ))
        .await
        .unwrap();

    let (owner, inherited) = repo
        .consume_and_insert("old-hash", "new-hash", Uuid::now_v7())
        .await
        .expect("原子轮换应成功")
        .expect("活跃旧枚应被抢到");
    assert_eq!(owner, admin_id, "应返回属主 admin_id");
    assert_eq!(
        inherited, old.expires_at,
        "返回的死线必须逐值等于旧枚的 expires_at（继承，不重算）"
    );

    let old_row = repo.find_by_hash("old-hash").await.unwrap().unwrap();
    assert!(old_row.rotated_at.is_some(), "旧枚应已标记轮换");
    assert!(old_row.revoked_at.is_none(), "轮换不应动 revoked_at");

    let new_row = repo.find_by_hash("new-hash").await.unwrap().unwrap();
    assert_eq!(new_row.admin_id, admin_id, "新枚属主应不变");
    assert!(
        new_row.rotated_at.is_none() && new_row.revoked_at.is_none(),
        "新枚应是活跃态"
    );
    assert_eq!(
        new_row.expires_at, old.expires_at,
        "新枚落库的 expires_at 必须 = 旧枚死线（Q8：轮换不续命）"
    );
}

/// 复用即拒：同一旧枚 consume 两次，第二次 None 且**不插入**第二枚新 token。
#[sqlx::test]
async fn consume_and_insert_second_time_misses_and_inserts_nothing(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);
    repo.insert(new_token(admin_id, "hash-once")).await.unwrap();

    repo.consume_and_insert("hash-once", "first-new", Uuid::now_v7())
        .await
        .unwrap()
        .expect("首次应抢到");

    let got = repo
        .consume_and_insert("hash-once", "second-new", Uuid::now_v7())
        .await
        .expect("落空不是错误");
    assert!(got.is_none(), "已消费的旧枚应落空");
    assert!(
        repo.find_by_hash("second-new").await.unwrap().is_none(),
        "落空时新枚绝不能落库（否则出现无主孤儿行）"
    );
}

/// 已吊销的旧枚不能被消费，且吊销行不得被误标 rotated。
#[sqlx::test]
async fn consume_and_insert_revoked_old_misses(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);
    repo.insert(new_token(admin_id, "hash-revoked"))
        .await
        .unwrap();
    repo.revoke_by_hash("hash-revoked").await.unwrap();

    let got = repo
        .consume_and_insert("hash-revoked", "should-not-exist", Uuid::now_v7())
        .await
        .unwrap();
    assert!(got.is_none(), "已吊销的不该能消费");

    let row = repo.find_by_hash("hash-revoked").await.unwrap().unwrap();
    assert!(row.rotated_at.is_none(), "吊销行不应被误盖 rotated_at");
    assert!(
        repo.find_by_hash("should-not-exist")
            .await
            .unwrap()
            .is_none(),
        "落空时不得插入新枚"
    );
}

/// 已过期的旧枚不能被消费（WHERE expires_at > NOW() 守住），行保持原样。
#[sqlx::test]
async fn consume_and_insert_expired_old_misses_and_leaves_it_untouched(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);
    repo.insert(new_token_expiring(
        admin_id,
        "hash-expired",
        Utc::now() - Duration::days(1),
    ))
    .await
    .unwrap();

    let got = repo
        .consume_and_insert("hash-expired", "should-not-exist", Uuid::now_v7())
        .await
        .unwrap();
    assert!(got.is_none(), "过期的不该能消费——死线到了必须重走 2FA");

    let row = repo.find_by_hash("hash-expired").await.unwrap().unwrap();
    assert!(row.rotated_at.is_none(), "过期行不应被误盖 rotated_at");
}

/// 不存在的旧哈希 → None（未命中是正常结果，不是错误）。
#[sqlx::test]
async fn consume_and_insert_unknown_old_misses(pool: PgPool) {
    let repo = AdminRefreshTokenRepository::new(pool);
    let got = repo
        .consume_and_insert("no-such-hash", "should-not-exist", Uuid::now_v7())
        .await
        .unwrap();
    assert!(got.is_none());
}

/// 并发抢同一枚旧 token：恰好一个抢到，且只有赢家的新枚落库。
#[sqlx::test]
async fn concurrent_consume_and_insert_only_one_wins(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);
    repo.insert(new_token(admin_id, "hash-race")).await.unwrap();

    let (a, b) = tokio::join!(
        repo.consume_and_insert("hash-race", "new-a", Uuid::now_v7()),
        repo.consume_and_insert("hash-race", "new-b", Uuid::now_v7()),
    );
    let (a, b) = (a.expect("不应报错"), b.expect("不应报错"));

    assert!(
        a.is_some() ^ b.is_some(),
        "并发下应恰好一个抢到（a={a:?}, b={b:?}）"
    );
    let winner_hash = if a.is_some() { "new-a" } else { "new-b" };
    let loser_hash = if a.is_some() { "new-b" } else { "new-a" };
    assert!(
        repo.find_by_hash(winner_hash).await.unwrap().is_some(),
        "赢家的新枚应落库"
    );
    assert!(
        repo.find_by_hash(loser_hash).await.unwrap().is_none(),
        "输家的新枚绝不能落库"
    );
}

/// **原子性**：INSERT 失败（新哈希撞唯一索引）→ 整体回滚，旧枚完好如初、之后仍可
/// 正常消费。钉的是「烧旧成功、发新失败」中间态的不可能性——CTE 单条 SQL 天然满足，
/// 拆成两条语句不同事务的实现会挂在 rotated_at 上。
#[sqlx::test]
async fn consume_and_insert_rolls_back_old_when_insert_fails(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);

    repo.insert(new_token(admin_id, "old-hash")).await.unwrap();
    // 预先占住 taken-hash，让 INSERT 必然撞唯一索引
    repo.insert(new_token(admin_id, "taken-hash")).await.unwrap();

    repo.consume_and_insert("old-hash", "taken-hash", Uuid::now_v7())
        .await
        .expect_err("撞唯一索引应报错");

    let old = repo.find_by_hash("old-hash").await.unwrap().unwrap();
    assert!(
        old.rotated_at.is_none() && old.revoked_at.is_none(),
        "INSERT 失败必须连带回滚 UPDATE——旧枚不能留在已轮换态"
    );

    // 活着要是真的：旧枚之后仍可正常消费（客户端重试即普通刷新）
    let (owner, _) = repo
        .consume_and_insert("old-hash", "retry-new", Uuid::now_v7())
        .await
        .unwrap()
        .expect("回滚后的旧枚应仍可消费");
    assert_eq!(owner, admin_id);
}

// ————————————————————— revoke_by_hash（logout）—————————————————————

/// 按哈希吊销：命中未吊销行 → 返回 1、盖 revoked_at、不动 rotated_at。
#[sqlx::test]
async fn revoke_by_hash_revokes_matching_row(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);
    repo.insert(new_token(admin_id, "hash-logout"))
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
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);
    repo.insert(new_token(admin_id, "hash-idem")).await.unwrap();

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

/// 不存在的哈希 → 0（幂等 logout 不因此报错）。
#[sqlx::test]
async fn revoke_by_hash_unknown_returns_zero(pool: PgPool) {
    let repo = AdminRefreshTokenRepository::new(pool);
    assert_eq!(repo.revoke_by_hash("no-such").await.unwrap(), 0);
}

// ————————————————— revoke_all_by_admin_id（单登录/重放连坐）—————————————————

/// 吊销该 admin 所有【未吊销】的 token，返回准确计数，且不波及别的 admin。
#[sqlx::test]
async fn revoke_all_by_admin_id_scopes_to_that_admin(pool: PgPool) {
    let alice = seed_admin(&pool).await;
    let bob = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);

    repo.insert(new_token(alice, "a1")).await.unwrap();
    repo.insert(new_token(alice, "a2")).await.unwrap();
    repo.insert(new_token(bob, "b1")).await.unwrap();

    let n = repo
        .revoke_all_by_admin_id(&alice)
        .await
        .expect("revoke_all 应成功");
    assert_eq!(n, 2, "应恰好吊销 alice 的 2 枚");

    for h in ["a1", "a2"] {
        let row = repo.find_by_hash(h).await.unwrap().unwrap();
        assert!(row.revoked_at.is_some(), "alice 的 {h} 应已吊销");
    }
    let bob_row = repo.find_by_hash("b1").await.unwrap().unwrap();
    assert!(bob_row.revoked_at.is_none(), "bob 的 token 不应被波及");
}

/// 幂等：已全吊销后再调返回 0，且不覆盖原吊销时刻。
#[sqlx::test]
async fn revoke_all_by_admin_id_is_idempotent(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool);
    repo.insert(new_token(admin_id, "h1")).await.unwrap();

    assert_eq!(repo.revoke_all_by_admin_id(&admin_id).await.unwrap(), 1);
    let first = repo
        .find_by_hash("h1")
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .expect("应已吊销");

    assert_eq!(
        repo.revoke_all_by_admin_id(&admin_id).await.unwrap(),
        0,
        "已全吊销后再调应返回 0（守卫生效）"
    );
    let second = repo
        .find_by_hash("h1")
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .expect("仍应是吊销态");
    assert_eq!(first, second, "重复调用不应刷新原吊销时刻");
}
