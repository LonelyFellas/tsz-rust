//! `AdminRepository` 的行为测试（真库，`#[sqlx::test]`）。
//!
//! 每个测试拿独立临时库、自动跑 `migrations/`、结束回滚。验的是**仓储层的 SQL 行为**，
//! 重点是 `register_failed_login` 的单条 UPDATE 状态机（admin-design.md §6，hardening-D8）：
//! 计数 +1 / 恰达阈值置锁且计数归零 / 顺手清 stale 锁 / 锁截止不被后续失败续期。
//!
//! ⚠️ 边界划分（重要）：repository **只搬 SQL**——
//!   - 不做 bcrypt：`password_hash` 存的就是入参原值，这里用普通字符串当"假哈希"。
//!   - 不执法「锁定中不累计」：那是 service 编排第①步（锁定检查先于一切因子）的活；
//!     锁定生效期间 repo 被误调的实际行为由本文件专门一条钉住，防止有人在 repo 层重复执法。
//!
//! 阈值/锁时长镜像 service 层常量（5 次 / 15 分钟）——service 侧改了这里要同步。

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use tsz_rust::admin::{
    Admin, AdminRepository, AdminRepositoryError, AdminRole, AdminStatus, NewAdmin,
};

const THRESHOLD: i32 = 5;

fn lock_for() -> Duration {
    Duration::minutes(15)
}

/// 唯一 phone 的新管理员载荷（phone 用 UUIDv7 串保证并行测试不撞唯一索引）。
fn new_admin() -> NewAdmin {
    let id = Uuid::now_v7();
    NewAdmin {
        id,
        phone: id.to_string(),
        display_name: "测试管理员".to_owned(),
        password_hash: "hashed-pw".to_owned(),
        role: AdminRole::Admin,
        must_change_password: true,
        created_by_admin_id: None,
    }
}

/// 建号并返回行快照。
async fn seed_admin(repo: &AdminRepository) -> Admin {
    repo.create(new_admin()).await.expect("seed admin 应成功")
}

/// 直改 locked_until——构造 stale/在锁场景用（绕过 repo 是刻意的：测试要摆出
/// repo 方法自己造不出的初始盘面）。
async fn set_locked_until(pool: &PgPool, id: Uuid, locked_until: Option<DateTime<Utc>>) {
    sqlx::query!(
        "UPDATE admins SET locked_until = $2 WHERE id = $1",
        id,
        locked_until,
    )
    .execute(pool)
    .await
    .expect("直改 locked_until 应成功");
}

// ————————————————————— create / get —————————————————————

/// 建号后按 id 查回：字段无损往返，锁定列是干净的出厂态（计数 0、无锁），时间戳已落。
#[sqlx::test]
async fn create_then_get_by_id_roundtrips(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let created = seed_admin(&repo).await;

    let found = repo.get_by_id(&created.id).await.expect("应能查回");
    assert_eq!(found.id, created.id);
    assert_eq!(found.phone, created.phone);
    assert_eq!(found.display_name, "测试管理员");
    assert_eq!(found.password_hash, "hashed-pw");
    assert_eq!(found.role, AdminRole::Admin);
    assert_eq!(found.status, AdminStatus::Active);
    assert!(found.must_change_password, "建号时显式传入的 true 应落库");

    assert_eq!(found.failed_login_count, 0, "出厂计数必须为 0");
    assert!(found.locked_until.is_none(), "出厂不得带锁");
    assert!(
        found.created_by_admin_id.is_none(),
        "seed/历史管理员默认没有后台创建人"
    );
    // 下界放宽到 -5s：created_at 由 DB 时钟落值，DB 快于测试机（远端库/CI 容器）时差为负不算缺陷
    let age = Utc::now() - found.created_at;
    assert!(
        age > Duration::seconds(-5) && age < Duration::seconds(10),
        "created_at 应是刚才: {:?}",
        found.created_at
    );
    assert_eq!(
        found.created_at, found.updated_at,
        "新行两个时间戳应一致（同一 DEFAULT NOW()）"
    );
}

/// super_admin 角色也能无损往返（枚举 TEXT 映射两个变体都过一遍）。
#[sqlx::test]
async fn create_super_admin_roundtrips_role(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let input = NewAdmin {
        role: AdminRole::SuperAdmin,
        must_change_password: false, // seed 纪律：超管显式 false（Q12）
        ..new_admin()
    };
    let created = repo.create(input).await.expect("create 应成功");
    assert_eq!(created.role, AdminRole::SuperAdmin);
    assert!(!created.must_change_password);
}

/// API 创建普通管理员时，创建人应从 INSERT RETURNING 以及两个查询入口完整带回。
#[sqlx::test]
async fn created_by_admin_id_roundtrips_through_repository(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let creator = repo
        .create(NewAdmin {
            role: AdminRole::SuperAdmin,
            must_change_password: false,
            ..new_admin()
        })
        .await
        .expect("创建超级管理员应成功");

    let created = repo
        .create(NewAdmin {
            created_by_admin_id: Some(creator.id),
            ..new_admin()
        })
        .await
        .expect("创建普通管理员应成功");
    assert_eq!(created.created_by_admin_id, Some(creator.id));

    let found_by_id = repo.get_by_id(&created.id).await.expect("按 id 应能查回");
    assert_eq!(found_by_id.created_by_admin_id, Some(creator.id));

    let found_by_phone = repo
        .get_by_phone(&created.phone)
        .await
        .expect("按 phone 应能查回");
    assert_eq!(found_by_phone.created_by_admin_id, Some(creator.id));
}

/// 撞已有手机号 → AlreadyExists（判重不先查，靠 admins_phone_key 唯一约束 + 23505 映射）。
#[sqlx::test]
async fn create_duplicate_phone_maps_to_already_exists(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let first = seed_admin(&repo).await;

    let dup = NewAdmin {
        phone: first.phone.clone(),
        ..new_admin()
    };
    let err = repo.create(dup).await.expect_err("重复 phone 应失败");
    assert!(
        matches!(err, AdminRepositoryError::AlreadyExists),
        "应映射成 AlreadyExists，而非透传 DB 错误: {err:?}"
    );
}

/// 未知 id → NotFound。
#[sqlx::test]
async fn get_by_id_unknown_is_not_found(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let err = repo
        .get_by_id(&Uuid::now_v7())
        .await
        .expect_err("未知 id 应失败");
    assert!(matches!(err, AdminRepositoryError::NotFound));
}

/// get_by_phone 精确匹配（admin 仅手机号标识，Q7/Q9——没有判 @ 的 identifier 语义）。
#[sqlx::test]
async fn get_by_phone_finds_exact_match(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let created = seed_admin(&repo).await;

    let found = repo
        .get_by_phone(&created.phone)
        .await
        .expect("按 phone 应能查回");
    assert_eq!(found.id, created.id);
}

/// 未知 phone → NotFound。
#[sqlx::test]
async fn get_by_phone_unknown_is_not_found(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let err = repo
        .get_by_phone("13800000000")
        .await
        .expect_err("未知 phone 应失败");
    assert!(matches!(err, AdminRepositoryError::NotFound));
}

// ————————————————————— set_password —————————————————————

/// hash 与 must_change 标志同条 UPDATE 原子落库，updated_at 前进。
#[sqlx::test]
async fn set_password_updates_hash_and_flag_atomically(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let created = seed_admin(&repo).await; // must_change = true

    repo.set_password(&created.id, "new-hash", false)
        .await
        .expect("set_password 应成功");

    let after = repo.get_by_id(&created.id).await.expect("应能查回");
    assert_eq!(after.password_hash, "new-hash");
    assert!(!after.must_change_password, "flag 应与 hash 同步清除");
    assert!(
        after.updated_at > created.updated_at,
        "UPDATE 必须显式推进 updated_at（无 trigger，靠 SQL 手写）"
    );
}

/// 未知 id → NotFound（rows_affected = 0 不得幽灵成功）。
#[sqlx::test]
async fn set_password_unknown_id_is_not_found(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let err = repo
        .set_password(&Uuid::now_v7(), "new-hash", false)
        .await
        .expect_err("未知 id 应失败");
    assert!(matches!(err, AdminRepositoryError::NotFound));
}

// ————————————————————— register_failed_login 状态机 —————————————————————

/// 阈值以下的失败：只累计、不置锁、返回 false。
#[sqlx::test]
async fn failures_below_threshold_only_increment(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let created = seed_admin(&repo).await;

    for i in 1..THRESHOLD {
        let locked = repo
            .register_failed_login(&created.id, THRESHOLD, lock_for())
            .await
            .expect("register_failed_login 应成功");
        assert!(!locked, "第 {i} 次失败（阈值 {THRESHOLD}）不应触锁");
    }

    let after = repo.get_by_id(&created.id).await.expect("应能查回");
    assert_eq!(after.failed_login_count, THRESHOLD - 1);
    assert!(after.locked_until.is_none(), "阈值以下绝不能出现锁");
    assert!(
        after.updated_at > created.updated_at,
        "失败累计也要推进 updated_at"
    );
}

/// 恰达阈值：返回 true、locked_until ≈ now + lock_for、计数归零（下个窗口满额重来）。
#[sqlx::test]
async fn threshold_failure_locks_and_resets_counter(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let created = seed_admin(&repo).await;

    for _ in 1..THRESHOLD {
        repo.register_failed_login(&created.id, THRESHOLD, lock_for())
            .await
            .expect("累计失败应成功");
    }
    let locked = repo
        .register_failed_login(&created.id, THRESHOLD, lock_for())
        .await
        .expect("第 5 次应成功");
    assert!(locked, "恰达阈值必须返回触锁");

    let after = repo.get_by_id(&created.id).await.expect("应能查回");
    assert_eq!(after.failed_login_count, 0, "触锁必须同时清零计数");
    let deadline = after.locked_until.expect("触锁必须落 locked_until");
    let remaining = deadline - Utc::now();
    assert!(
        remaining > Duration::minutes(14) && remaining <= Duration::minutes(15),
        "锁截止应 ≈ now + 15min，实际剩余 {remaining}"
    );
}

/// 锁定生效期间再失败：现锁**冻结**——截止不得被续期（攻击者不能把解锁时间无限后推，
/// §6①），计数也不得累计（否则 ①锁中每攒满 threshold 次就把锁续期一次 ②锁内攒下的
/// 计数蚕食到期后新窗口的满额配额）。误调次数特意 ≥ threshold——只误调 1 次逮不住
/// 「锁中再次达阈值触发续期」的分支。
/// 注：按编排 service 在锁定中根本不会调本方法（锁定检查先于一切因子），此处钉的是
/// repo 被误调时的兜底行为。
#[sqlx::test]
async fn active_lock_deadline_is_never_extended(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let created = seed_admin(&repo).await;

    for _ in 0..THRESHOLD {
        repo.register_failed_login(&created.id, THRESHOLD, lock_for())
            .await
            .expect("触锁流程应成功");
    }
    let deadline_before = repo
        .get_by_id(&created.id)
        .await
        .expect("应能查回")
        .locked_until
        .expect("此刻应在锁中");

    // 锁中连续误调 threshold + 1 次：覆盖「再次攒满阈值」的续期路径
    for i in 1..=(THRESHOLD + 1) {
        let locked = repo
            .register_failed_login(&created.id, THRESHOLD, lock_for())
            .await
            .expect("锁中误调也应成功");
        assert!(locked, "锁定生效期间应报告仍在锁中（第 {i} 次误调）");
    }

    let after = repo.get_by_id(&created.id).await.expect("应能查回");
    assert_eq!(
        after.locked_until,
        Some(deadline_before),
        "锁截止必须原样保持——锁中误调任意次（含再次攒满阈值）都不得续期"
    );
    assert_eq!(
        after.failed_login_count, 0,
        "锁中冻结计数——锁内累计会蚕食到期后新窗口的满额配额"
    );
}

/// 过期的 stale 锁被下一次失败顺手清掉，且下个窗口拥有满额配额（从 1 重新数到 5 才再触锁）。
#[sqlx::test]
async fn stale_lock_is_cleared_and_window_resets(pool: PgPool) {
    let repo = AdminRepository::new(pool.clone());
    let created = seed_admin(&repo).await;

    // 触锁后把锁回拨成已过期（构造"锁自然到期"盘面）
    for _ in 0..THRESHOLD {
        repo.register_failed_login(&created.id, THRESHOLD, lock_for())
            .await
            .expect("触锁流程应成功");
    }
    set_locked_until(&pool, created.id, Some(Utc::now() - Duration::seconds(1))).await;

    // 新窗口第 1 次失败：不触锁 + stale 锁被清
    let locked = repo
        .register_failed_login(&created.id, THRESHOLD, lock_for())
        .await
        .expect("应成功");
    assert!(!locked, "stale 锁不算在锁中，新窗口第 1 次失败不触锁");
    let after = repo.get_by_id(&created.id).await.expect("应能查回");
    assert!(after.locked_until.is_none(), "过期锁必须被顺手清成 NULL");
    assert_eq!(after.failed_login_count, 1);

    // 满额配额：再数 3 次仍不锁，第 5 次才再触锁
    for _ in 0..(THRESHOLD - 2) {
        let locked = repo
            .register_failed_login(&created.id, THRESHOLD, lock_for())
            .await
            .expect("应成功");
        assert!(!locked, "新窗口阈值以下不应触锁");
    }
    let locked = repo
        .register_failed_login(&created.id, THRESHOLD, lock_for())
        .await
        .expect("应成功");
    assert!(locked, "新窗口第 5 次应再次触锁");
}

/// 恰好 THRESHOLD 路并发失败：单条 UPDATE 靠行锁串行化，只有最后到达者见到计数攒满，
/// 故**恰好一个**调用报告在锁中，终态=锁中+计数 0。
/// 注意返回值语义是「此刻是否在锁中」而非「本次是否触锁」——并发路数若超过 THRESHOLD，
/// 锁后到达者也会返回 true；本测试刻意把路数钉在 THRESHOLD 上，别加路数。
#[sqlx::test]
async fn concurrent_failures_trigger_exactly_one_lock(pool: PgPool) {
    let repo = AdminRepository::new(pool.clone());
    let created = seed_admin(&repo).await;

    let mut handles = Vec::new();
    for _ in 0..THRESHOLD {
        let task_repo = AdminRepository::new(pool.clone());
        let id = created.id;
        handles.push(tokio::spawn(async move {
            task_repo
                .register_failed_login(&id, THRESHOLD, lock_for())
                .await
        }));
    }
    let mut in_lock = 0;
    for handle in handles {
        let locked = handle
            .await
            .expect("并发任务不应 panic")
            .expect("并发失败登记应全部成功");
        if locked {
            in_lock += 1;
        }
    }
    assert_eq!(in_lock, 1, "THRESHOLD 路并发中只有最后到达者见到锁");

    let after = repo.get_by_id(&created.id).await.expect("应能查回");
    assert_eq!(after.failed_login_count, 0, "触锁清零");
    assert!(after.locked_until.is_some(), "终态应在锁中");
}

/// 未知 id → NotFound（rows_affected = 0 不得幽灵成功）。
#[sqlx::test]
async fn register_failed_login_unknown_id_is_not_found(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let err = repo
        .register_failed_login(&Uuid::now_v7(), THRESHOLD, lock_for())
        .await
        .expect_err("未知 id 应失败");
    assert!(matches!(err, AdminRepositoryError::NotFound));
}

// ————————————————————— clear_failed_login —————————————————————

/// 登录成功清场：计数与锁**双列**一次清零。
#[sqlx::test]
async fn clear_failed_login_resets_both_columns(pool: PgPool) {
    let repo = AdminRepository::new(pool.clone());
    let created = seed_admin(&repo).await;

    for _ in 0..3 {
        repo.register_failed_login(&created.id, THRESHOLD, lock_for())
            .await
            .expect("累计失败应成功");
    }
    // 手工摆一个在锁（repo 流程造不出"计数>0 且带锁"的盘面）
    set_locked_until(&pool, created.id, Some(Utc::now() + Duration::minutes(5))).await;

    repo.clear_failed_login(&created.id)
        .await
        .expect("clear 应成功");

    let after = repo.get_by_id(&created.id).await.expect("应能查回");
    assert_eq!(after.failed_login_count, 0);
    assert!(after.locked_until.is_none(), "锁列必须一并清除");
}

/// 未知 id → NotFound（rows_affected = 0 不得幽灵成功）。
#[sqlx::test]
async fn clear_failed_login_unknown_id_is_not_found(pool: PgPool) {
    let repo = AdminRepository::new(pool);
    let err = repo
        .clear_failed_login(&Uuid::now_v7())
        .await
        .expect_err("未知 id 应失败");
    assert!(matches!(err, AdminRepositoryError::NotFound));
}
