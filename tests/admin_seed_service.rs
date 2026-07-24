//! `AdminService::seed_super_admin` 的行为测试（真库，`#[sqlx::test]`）。
//!
//! seed 的契约（cmd: src/bin/seed.rs）——**只创建超管，绝不改动已存在账号**：
//!   - 空库/新 phone → 新建 active 超管，`must_change_password=false`（密码是人挑的）；
//!   - 手机号已是超管 → `Unchanged`，一个字段都不动（超管恒 active，无需自愈；密码/昵称不覆盖）；
//!   - 手机号被普通 admin 占用 → 拒绝（`PhoneTakenByNonSuperAdmin`），账号原样不动
//!     （seed 不擅自提级，否则打错手机号即误提级）；
//!   - 重跑对同一超管幂等：永不报错、永不建第二个账号。
//!
//! ⚠️ 「永不覆盖已存在账号」是这组测试的重点——无论 Unchanged 还是拒绝，
//! password_hash / display_name / role 都要钉原样。

use sqlx::PgPool;
use uuid::Uuid;

use tsz_rust::admin::{
    AdminRepository, AdminRole, AdminSeedError, AdminService, AdminStatus, NewAdmin, SeedOutcome,
};
use tsz_rust::platform::{PasswordError, verify_password};

const PASSWORD: &str = "S3cure-Pa55word";
/// 存量账号的"假哈希"——repository 不做 bcrypt，原样落库，正好用来断言"未被覆盖"。
const EXISTING_HASH: &str = "existing-hash";
const EXISTING_NAME: &str = "存量管理员";

fn service(pool: &PgPool) -> AdminService {
    AdminService::for_seed(AdminRepository::new(pool.clone()))
}

/// 造一个指定 role 的 active 存量管理员，返回 (id, phone)。
/// phone 用 UUIDv7 串保证并行测试不撞唯一索引。
async fn seed_existing(pool: &PgPool, role: AdminRole) -> (Uuid, String) {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.to_string(),
            display_name: EXISTING_NAME.to_owned(),
            password_hash: EXISTING_HASH.to_owned(),
            role,
            must_change_password: true,
        })
        .await
        .expect("造存量管理员应成功");
    (id, id.to_string())
}

#[sqlx::test]
async fn creates_active_super_admin_on_fresh_db(pool: PgPool) {
    let outcome = service(&pool)
        .seed_super_admin("13800138000", PASSWORD, "超管")
        .await
        .expect("空库 seed 应成功");

    let SeedOutcome::Created(admin) = outcome else {
        panic!("空库应走 Created，实际 {outcome:?}");
    };
    assert_eq!(admin.role, AdminRole::SuperAdmin);
    assert_eq!(admin.status, AdminStatus::Active);
    assert!(
        !admin.must_change_password,
        "seed 的密码是人挑的，不应强制首登改密（表默认 TRUE 是给 Provision 的）"
    );
    assert!(
        verify_password(PASSWORD.to_string(), admin.password_hash.clone()).await,
        "落库哈希应能验证原密码"
    );
}

#[sqlx::test]
async fn rerun_is_idempotent(pool: PgPool) {
    let svc = service(&pool);
    let first = svc
        .seed_super_admin("13800138000", PASSWORD, "超管")
        .await
        .expect("首跑应成功");
    let SeedOutcome::Created(created) = first else {
        panic!("首跑应走 Created，实际 {first:?}");
    };

    let second = svc
        .seed_super_admin("13800138000", PASSWORD, "超管")
        .await
        .expect("重跑应成功");
    let SeedOutcome::Unchanged(unchanged) = second else {
        panic!("重跑应走 Unchanged，实际 {second:?}");
    };
    assert_eq!(created.id, unchanged.id, "重跑不得建第二个账号");
}

/// 手机号被**普通 admin** 占用 → 拒绝，且账号原样不动（不提级、不改任何字段）。
/// 这是刻意的最小惊讶设计：seed 只创建超管，打错手机号也不会把某个普通 admin 提成超管。
#[sqlx::test]
async fn refuses_to_promote_existing_plain_admin(pool: PgPool) {
    let (id, phone) = seed_existing(&pool, AdminRole::Admin).await;

    let err = service(&pool)
        .seed_super_admin(&phone, PASSWORD, "无关紧要")
        .await
        .expect_err("对普通 admin 应拒绝，不应提级");
    assert!(
        matches!(err, AdminSeedError::PhoneTakenByNonSuperAdmin),
        "应是 PhoneTakenByNonSuperAdmin，实际 {err:?}"
    );

    // 账号必须原样——回读验证 seed 一个字段都没动
    let admin = AdminRepository::new(pool.clone())
        .get_by_id(&id)
        .await
        .expect("回读应成功");
    assert_eq!(
        admin.role,
        AdminRole::Admin,
        "普通 admin 绝不能被 seed 提级"
    );
    assert_eq!(admin.password_hash, EXISTING_HASH, "密码不得被动");
    assert_eq!(admin.display_name, EXISTING_NAME, "昵称不得被动");
}

#[sqlx::test]
async fn existing_super_admin_is_left_untouched(pool: PgPool) {
    let (id, phone) = seed_existing(&pool, AdminRole::SuperAdmin).await;

    // 故意传不同的密码和昵称——Unchanged 分支必须对存量数据零写入
    let outcome = service(&pool)
        .seed_super_admin(&phone, "Another-Pa55word", "别的名字")
        .await
        .expect("对已存在超管 seed 应成功");
    assert!(
        matches!(outcome, SeedOutcome::Unchanged(_)),
        "已存在超管应走 Unchanged，实际 {outcome:?}"
    );

    let admin = AdminRepository::new(pool.clone())
        .get_by_id(&id)
        .await
        .expect("回读应成功");
    assert_eq!(admin.password_hash, EXISTING_HASH, "重跑 seed 不得重置密码");
    assert_eq!(admin.display_name, EXISTING_NAME, "昵称也不应被覆盖");
    assert!(
        admin.must_change_password,
        "存量账号的 must_change_password 不归 seed 管"
    );
}

#[sqlx::test]
async fn rejects_weak_password_on_create(pool: PgPool) {
    let err = service(&pool)
        .seed_super_admin("13800138001", "short", "超管")
        .await
        .expect_err("弱密码应被拒");
    assert!(
        matches!(err, AdminSeedError::Password(PasswordError::TooShort)),
        "应透传具体的策略错误（TooShort），实际 {err:?}"
    );
}
