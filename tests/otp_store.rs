//! `OtpStore`（Redis 版）集成测试 —— OTP 域从 Postgres 迁到 Redis 的验收标准。
//! 对齐 docs/redis-design.md §5（码存储）/§6（校验+单次消费 Lua）/§8（冷却+滚动日限）/§10（测试）。
//!
//! 真 Redis、不 mock（同 `#[sqlx::test]` 用真库的哲学）。隔离用 **key 前缀**（§10 决策 E）：
//! 每个用例一个唯一前缀 → 可并行、无需 FLUSHDB、测完不用清（键都带 TTL）。
//!
//! ── 本文件同时定义 `OtpStore` 的**待实现契约**（方法签名 + key 方案）──
//!
//! ## OtpStore 契约（待实现）
//! ```ignore
//! use std::time::Duration;   // ⚠️ std（非负、天然「时间跨度」语义），非 chrono::Duration。
//!                            //    Redis EXPIRE/EX/score 只吃非负整数秒；实现用 Duration::as_secs()。
//! pub struct OtpStore { /* redis: Pool, prefix: String */ }
//! impl OtpStore {
//!     pub fn new(redis: deadpool_redis::Pool) -> Self;                                    // 生产：prefix = ""
//!     pub fn with_prefix(redis: deadpool_redis::Pool, prefix: impl Into<String>) -> Self; // 测试
//!
//!     // 存码：HSET {code, attempts=0} + 整键 EXPIRE ttl。同 target+purpose 再存 = **整键覆盖**：
//!     //   code 换新、attempts 归零、TTL 重设（不得继承旧键剩余 TTL）。「只认最新」由此天然成立。
//!     pub async fn save_code(&self, target: &str, purpose: Purpose, code: &str, ttl: Duration) -> Result<(), OtpStoreError>;
//!
//!     // 校验 + 单次消费（**一段 Lua 原子**）：
//!     //   命中 → DEL 该键，返 true；不中 → HINCRBY attempts，达 max 则 DEL 锁死，返 false；
//!     //   没发过/已过期/已消费/已锁死 → 一律 false（store 层不可区分，统一在 service 保证）。
//!     pub async fn verify_code(&self, target: &str, purpose: Purpose, submitted: &str, max_attempts: u32) -> Result<bool, OtpStoreError>;
//!
//!     // 冷却（**SET NX EX**，原子 check-and-mark）：true = 放行（已占位）；false = 冷却中。
//!     pub async fn check_cooldown(&self, target: &str, purpose: Purpose, cooldown: Duration) -> Result<bool, OtpStoreError>;
//!
//!     // 滚动日限（ZSet + **一段 Lua 原子**：修剪窗外 → 计数 → 达 limit 返 false 不追加 / 否则 ZADD 记录
//!     //   + EXPIRE 自清理，返 true）。member 用 uuid（唯一，防同刻覆盖少计）；score = **unix 秒**。
//!     pub async fn check_daily_limit(&self, target: &str, purpose: Purpose, limit: u64, window: Duration) -> Result<bool, OtpStoreError>;
//! }
//! ```
//!
//! ## Key 方案（实现必须与此一致，否则本文件直接读写 Redis 的断言会失配）
//! - 码：    `{prefix}otp:code:{target}:{seg}`   Hash，字段 `code` / `attempts`
//! - 冷却：  `{prefix}otp:cd:{target}:{seg}`     String（SET NX EX）
//! - 日限：  `{prefix}otp:daily:{target}:{seg}`  Sorted Set，score = unix 秒
//! - `{seg}` = `Purpose::as_key_segment()`（snake_case，单一来源，test 与实现共用同一函数）
//!
//! ## 时钟
//! 日限窗口的「现在」由实现取 **Redis 服务器时钟**（Lua 内 `redis.call('TIME')`）——单一时钟、无
//! app↔Redis 偏差；Redis 7 默认 effects replication，`TIME` 后接写命令安全。修剪测试用 app 时钟播种
//! 一条「远早于窗口」的旧事件（差值 ~28h ≫ 任何同机时钟偏差），故对两套时钟的细微差不敏感。

use chrono::Utc;
use deadpool_redis::{Pool, redis};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use tsz_rust::otp::model::Purpose;
use tsz_rust::otp::store::OtpStore;
use tsz_rust::platform;

// —— 常量 ——
const T: &str = "13800000000";
const T2: &str = "13900000000";

// —— 测试脚手架 ——

fn test_redis_url() -> String {
    std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_owned())
}

/// 建一个独享唯一前缀的 store，并返回同一个 pool（供个别用例直接读写 Redis 内部状态）。
async fn fresh_store() -> (OtpStore, Pool, String) {
    let pool = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let prefix = format!("test:{}:", Uuid::now_v7());
    let store = OtpStore::with_prefix(pool.clone(), prefix.clone());
    (store, pool, prefix)
}

// key 构造：**用与实现同一个 `Purpose::as_key_segment()`**，编译期保证 test 与实现不漂移。
fn code_key(prefix: &str, target: &str, p: Purpose) -> String {
    format!("{prefix}otp:code:{target}:{}", p.as_key_segment())
}
fn cd_key(prefix: &str, target: &str, p: Purpose) -> String {
    format!("{prefix}otp:cd:{target}:{}", p.as_key_segment())
}
fn daily_key(prefix: &str, target: &str, p: Purpose) -> String {
    format!("{prefix}otp:daily:{target}:{}", p.as_key_segment())
}

async fn read_ttl(pool: &Pool, key: &str) -> i64 {
    let mut conn = pool.get().await.unwrap();
    redis::cmd("TTL")
        .arg(key)
        .query_async(&mut conn)
        .await
        .unwrap()
}
async fn zcard(pool: &Pool, key: &str) -> i64 {
    let mut conn = pool.get().await.unwrap();
    redis::cmd("ZCARD")
        .arg(key)
        .query_async(&mut conn)
        .await
        .unwrap()
}
async fn exists(pool: &Pool, key: &str) -> bool {
    let mut conn = pool.get().await.unwrap();
    let n: i64 = redis::cmd("EXISTS")
        .arg(key)
        .query_async(&mut conn)
        .await
        .unwrap();
    n == 1
}

// ============================ §5 码存储 ============================

#[tokio::test]
async fn save_then_verify_roundtrips() {
    let (store, ..) = fresh_store().await;
    store
        .save_code(T, Purpose::Login, "123456", Duration::from_secs(300))
        .await
        .unwrap();

    let ok = store
        .verify_code(T, Purpose::Login, "123456", 5)
        .await
        .unwrap();
    assert!(ok, "刚存的正确码应校验通过");
}

#[tokio::test]
async fn save_sets_ttl_near_requested() {
    let (store, pool, prefix) = fresh_store().await;
    store
        .save_code(T, Purpose::Login, "123456", Duration::from_secs(300))
        .await
        .unwrap();

    // 断言 TTL 贴近请求值（295..=300），而非只 >0：后者放行「把 minutes(5) 当 5 秒」这类单位 bug（假绿）。
    let ttl = read_ttl(&pool, &code_key(&prefix, T, Purpose::Login)).await;
    assert!(
        (295..=300).contains(&ttl),
        "码键 TTL 应贴近 300s（允许少量流逝），实得 {ttl}（-1=永久未设过期，-2=键不存在）"
    );
}

#[tokio::test]
async fn newer_code_overwrites_older() {
    let (store, ..) = fresh_store().await;
    store
        .save_code(T, Purpose::Login, "111111", Duration::from_secs(300))
        .await
        .unwrap();
    store
        .save_code(T, Purpose::Login, "222222", Duration::from_secs(300))
        .await
        .unwrap();

    assert!(
        !store
            .verify_code(T, Purpose::Login, "111111", 5)
            .await
            .unwrap(),
        "被覆盖的旧码应失效"
    );
    assert!(
        store
            .verify_code(T, Purpose::Login, "222222", 5)
            .await
            .unwrap(),
        "最新码应有效"
    );
}

#[tokio::test]
async fn overwrite_resets_attempts() {
    let (store, ..) = fresh_store().await;
    // 旧码累计 2 次错误（max=3，未锁）。
    store
        .save_code(T, Purpose::Login, "111111", Duration::from_secs(300))
        .await
        .unwrap();
    for _ in 0..2 {
        assert!(
            !store
                .verify_code(T, Purpose::Login, "000000", 3)
                .await
                .unwrap()
        );
    }
    // 覆盖成新码——attempts 必须随之归零。
    store
        .save_code(T, Purpose::Login, "222222", Duration::from_secs(300))
        .await
        .unwrap();

    // 覆盖后再错一次不该锁死（若 attempts 未归零，这次会把计数推到 3 触发 DEL）。
    assert!(
        !store
            .verify_code(T, Purpose::Login, "000000", 3)
            .await
            .unwrap(),
        "错码仍失败"
    );
    assert!(
        store
            .verify_code(T, Purpose::Login, "222222", 3)
            .await
            .unwrap(),
        "覆盖须把 attempts 归零，否则旧码累计的错误会误锁新码、正确码失效"
    );
}

#[tokio::test]
async fn overwrite_refreshes_ttl() {
    let (store, pool, prefix) = fresh_store().await;
    // 旧码短 TTL，新码长 TTL：覆盖须**重设** EXPIRE，而非继承旧键剩余的 30s。
    store
        .save_code(T, Purpose::Login, "111111", Duration::from_secs(30))
        .await
        .unwrap();
    store
        .save_code(T, Purpose::Login, "222222", Duration::from_secs(300))
        .await
        .unwrap();

    let ttl = read_ttl(&pool, &code_key(&prefix, T, Purpose::Login)).await;
    assert!(
        ttl > 60,
        "覆盖应重设 EXPIRE（新 ttl=300），而非继承旧码的 30s；实得 {ttl}"
    );
}

// ======================= §6 校验 + 单次消费 =======================

#[tokio::test]
async fn correct_code_is_single_use() {
    let (store, ..) = fresh_store().await;
    store
        .save_code(T, Purpose::Login, "123456", Duration::from_secs(300))
        .await
        .unwrap();

    assert!(
        store
            .verify_code(T, Purpose::Login, "123456", 5)
            .await
            .unwrap(),
        "首次正确码应通过"
    );
    assert!(
        !store
            .verify_code(T, Purpose::Login, "123456", 5)
            .await
            .unwrap(),
        "再验应失败（单次消费）"
    );
}

#[tokio::test]
async fn never_sent_code_is_invalid() {
    let (store, ..) = fresh_store().await;
    assert!(
        !store
            .verify_code(T, Purpose::Login, "123456", 5)
            .await
            .unwrap(),
        "没发过码的 target 校验应失败"
    );
}

#[tokio::test]
async fn empty_submission_is_invalid_but_does_not_consume() {
    let (store, ..) = fresh_store().await;
    store
        .save_code(T, Purpose::Login, "123456", Duration::from_secs(300))
        .await
        .unwrap();

    // 空串应走「不中」而非因字段缺失误判命中；且不得销毁码。
    assert!(
        !store.verify_code(T, Purpose::Login, "", 3).await.unwrap(),
        "空串不应命中"
    );
    assert!(
        store
            .verify_code(T, Purpose::Login, "123456", 3)
            .await
            .unwrap(),
        "空串不应消耗掉正确码"
    );
}

#[tokio::test]
async fn wrong_attempts_below_max_do_not_lock() {
    let (store, ..) = fresh_store().await;
    store
        .save_code(T, Purpose::Login, "123456", Duration::from_secs(300))
        .await
        .unwrap();

    for _ in 0..2 {
        assert!(
            !store
                .verify_code(T, Purpose::Login, "000000", 3)
                .await
                .unwrap(),
            "错码应失败"
        );
    }
    assert!(
        store
            .verify_code(T, Purpose::Login, "123456", 3)
            .await
            .unwrap(),
        "未达上限前正确码仍应通过"
    );
}

#[tokio::test]
async fn code_locks_and_is_deleted_after_max_attempts() {
    let (store, pool, prefix) = fresh_store().await;
    store
        .save_code(T, Purpose::Login, "123456", Duration::from_secs(300))
        .await
        .unwrap();

    for _ in 0..3 {
        assert!(
            !store
                .verify_code(T, Purpose::Login, "000000", 3)
                .await
                .unwrap(),
            "错码应失败"
        );
    }
    assert!(
        !store
            .verify_code(T, Purpose::Login, "123456", 3)
            .await
            .unwrap(),
        "错满 max 后正确码也应失效（锁死）"
    );
    // 锁死须**整键 DEL**（而非只删 code 字段留 attempts 残留），否则靠原 TTL 才不泄漏。
    assert!(
        !exists(&pool, &code_key(&prefix, T, Purpose::Login)).await,
        "锁死后码键应被整键删除，无残留"
    );
}

#[tokio::test]
async fn single_attempt_max_locks_on_first_wrong() {
    let (store, ..) = fresh_store().await;
    // max_attempts=1 边界：单次错验即锁。
    store
        .save_code(T, Purpose::Login, "123456", Duration::from_secs(300))
        .await
        .unwrap();
    assert!(
        !store
            .verify_code(T, Purpose::Login, "000000", 1)
            .await
            .unwrap(),
        "错码应失败"
    );
    assert!(
        !store
            .verify_code(T, Purpose::Login, "123456", 1)
            .await
            .unwrap(),
        "max=1 时一次错即锁，正确码随后失效"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_verify_of_correct_code_only_one_wins() {
    let (store, ..) = fresh_store().await;
    store
        .save_code(T, Purpose::Login, "123456", Duration::from_secs(300))
        .await
        .unwrap();

    // 真并发（多线程 + 多 future）压 verify+DEL 的原子性：非原子实现（HGET→Rust 比对→DEL）会有
    // 多个 true。仅 2 个 future 不足以逼出交错——单次消费本身就保证 wins==1，遮住原子性 bug。
    let store = Arc::new(store);
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..24 {
        let s = store.clone();
        set.spawn(async move { s.verify_code(T, Purpose::Login, "123456", 5).await.unwrap() });
    }
    let mut wins = 0;
    while let Some(res) = set.join_next().await {
        if res.unwrap() {
            wins += 1;
        }
    }
    assert_eq!(wins, 1, "并发校验同一正确码应恰好一个通过，实得 {wins}");
}

// ============================ §8 冷却 ============================

#[tokio::test]
async fn cooldown_blocks_second_request_in_window() {
    let (store, ..) = fresh_store().await;
    assert!(
        store
            .check_cooldown(T, Purpose::Login, Duration::from_secs(60))
            .await
            .unwrap(),
        "首次应放行"
    );
    assert!(
        !store
            .check_cooldown(T, Purpose::Login, Duration::from_secs(60))
            .await
            .unwrap(),
        "窗内再次应被挡"
    );
}

#[tokio::test]
async fn cooldown_sets_ttl() {
    let (store, pool, prefix) = fresh_store().await;
    assert!(
        store
            .check_cooldown(T, Purpose::Login, Duration::from_secs(60))
            .await
            .unwrap()
    );

    // 冷却键必须带过期（SET NX **EX**）。漏 EX = 永久键 = 该 target+purpose 永远发不出码。
    let ttl = read_ttl(&pool, &cd_key(&prefix, T, Purpose::Login)).await;
    assert!(
        (1..=60).contains(&ttl),
        "冷却键 TTL 应落在 (0,60]，实得 {ttl}（-1=永久封锁 bug）"
    );
}

#[tokio::test]
async fn cooldown_is_independent_per_purpose() {
    let (store, ..) = fresh_store().await;
    assert!(
        store
            .check_cooldown(T, Purpose::Login, Duration::from_secs(60))
            .await
            .unwrap()
    );
    assert!(
        store
            .check_cooldown(T, Purpose::PasswordReset, Duration::from_secs(60))
            .await
            .unwrap(),
        "改密码冷却应与登录独立"
    );
}

#[tokio::test]
async fn cooldown_is_independent_per_target() {
    let (store, ..) = fresh_store().await;
    let a = store
        .check_cooldown(T, Purpose::Login, Duration::from_secs(60))
        .await
        .unwrap();
    let b = store
        .check_cooldown(T2, Purpose::Login, Duration::from_secs(60))
        .await
        .unwrap();
    assert!(a && b, "不同 target 的冷却应互不影响");
}

// ==================== §8 滚动日限（Sorted Set） ====================

const DAY: Duration = Duration::from_secs(86400);

#[tokio::test]
async fn daily_limit_allows_up_to_limit_then_blocks() {
    let (store, pool, prefix) = fresh_store().await;

    for i in 1..=3 {
        assert!(
            store
                .check_daily_limit(T, Purpose::Login, 3, DAY)
                .await
                .unwrap(),
            "第 {i} 次应放行"
        );
    }
    // 每次放行都记一条**不同** member（uuid），故 ZCARD==3；若 member 碰撞会少计。
    let dkey = daily_key(&prefix, T, Purpose::Login);
    assert_eq!(
        zcard(&pool, &dkey).await,
        3,
        "3 次放行应各记一条，ZCARD 应为 3"
    );

    // 达 limit 后被挡，且**不追加**（否则 ZSet 无界膨胀）。
    assert!(
        !store
            .check_daily_limit(T, Purpose::Login, 3, DAY)
            .await
            .unwrap(),
        "第 4 次应被挡"
    );
    assert!(
        !store
            .check_daily_limit(T, Purpose::Login, 3, DAY)
            .await
            .unwrap(),
        "第 5 次应被挡"
    );
    assert_eq!(
        zcard(&pool, &dkey).await,
        3,
        "被挡的请求不得追加，ZCARD 应仍为 3"
    );
}

#[tokio::test]
async fn daily_limit_sets_ttl_on_zset() {
    let (store, pool, prefix) = fresh_store().await;
    assert!(
        store
            .check_daily_limit(T, Purpose::Login, 3, DAY)
            .await
            .unwrap()
    );

    // ZSet 须设 EXPIRE 自清理（§8「防内存泄漏」）——否则每个来过的 target 永久留一个 ZSet。
    let ttl = read_ttl(&pool, &daily_key(&prefix, T, Purpose::Login)).await;
    assert!(
        ttl > 0 && ttl <= 86400,
        "日限 ZSet TTL 应落在 (0,86400]，实得 {ttl}"
    );
}

#[tokio::test]
async fn daily_limit_trims_entries_outside_window() {
    let (store, pool, prefix) = fresh_store().await;

    // 直接塞一条「窗外」旧事件（score 远早于 24h 窗口），验证滚动窗口会修剪掉、不计入当前计数。
    // 用不着 sleep（对齐 §10「直接操纵状态」）；差值 ~28h ≫ 任何同机时钟偏差。
    let dkey = daily_key(&prefix, T, Purpose::Login);
    let old_score = Utc::now().timestamp() - 100_000;
    {
        let mut conn = pool.get().await.unwrap();
        redis::cmd("ZADD")
            .arg(&dkey)
            .arg(old_score)
            .arg("stale-event")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();
    }

    // limit=1：旧事件若被正确修剪 → 窗内计数 0 < 1 → 放行。漏修剪则计数 1>=1 → 误挡，本断言失败。
    assert!(
        store
            .check_daily_limit(T, Purpose::Login, 1, DAY)
            .await
            .unwrap(),
        "窗外旧事件应被修剪、不占额度，本次应放行"
    );
}

#[tokio::test]
async fn daily_limit_is_independent_per_purpose() {
    let (store, ..) = fresh_store().await;
    for _ in 0..3 {
        store
            .check_daily_limit(T, Purpose::Login, 3, DAY)
            .await
            .unwrap();
    }
    assert!(
        store
            .check_daily_limit(T, Purpose::PasswordReset, 3, DAY)
            .await
            .unwrap(),
        "改密码日限应与登录独立，不被其占额挤掉"
    );
}

#[tokio::test]
async fn daily_limit_is_independent_per_target() {
    let (store, ..) = fresh_store().await;
    for _ in 0..3 {
        store
            .check_daily_limit(T, Purpose::Login, 3, DAY)
            .await
            .unwrap();
    }
    assert!(
        store
            .check_daily_limit(T2, Purpose::Login, 3, DAY)
            .await
            .unwrap(),
        "不同 target 的日限应独立"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_daily_limit_admits_exactly_limit() {
    let (store, ..) = fresh_store().await;

    // limit=1 并发 10 个：若实现把 ZCARD→判断→ZADD 拆成多条命令（非 Lua 原子），并发会同时读到
    // n<limit 各自 ZADD、冲破上限。恰好放行 1 个才证明 check-then-act 被 Lua 原子性堵住（§8 坑 1）。
    let store = Arc::new(store);
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let s = store.clone();
        set.spawn(async move {
            s.check_daily_limit(T, Purpose::Login, 1, DAY)
                .await
                .unwrap()
        });
    }
    let mut allowed = 0;
    while let Some(res) = set.join_next().await {
        if res.unwrap() {
            allowed += 1;
        }
    }
    assert_eq!(allowed, 1, "limit=1 并发应恰好放行 1 个，实得 {allowed}");
}
