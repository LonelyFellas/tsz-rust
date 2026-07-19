//! admins / admin_refresh_tokens 表的 schema 约束测试(admin-design.md §3)。
//! 红灯驱动:迁移落地前全部失败(表不存在),落地后应全绿。
//! 验证:默认值、phone 唯一、email 大小写不敏感部分唯一、level/status CHECK、
//! token_hash 唯一、FK 级联、以及「与 web 身份库完全隔离」的两条边界断言。

use sqlx::PgPool;
use uuid::Uuid;

/// 断言操作败于**特定约束**(按 PG 错误码),而非任意错误——
/// 否则「表不存在」也算 is_err(),约束测试在迁移缺失时会假绿。
/// 码表:23505=unique_violation, 23514=check_violation, 23503=foreign_key_violation。
fn assert_db_error_code<T: std::fmt::Debug>(
    result: Result<T, sqlx::Error>,
    expected_code: &str,
    msg: &str,
) {
    match result {
        Err(sqlx::Error::Database(db)) => {
            assert_eq!(
                db.code().as_deref(),
                Some(expected_code),
                "{msg}(错误码不符,实际:{:?} - {})",
                db.code(),
                db.message()
            );
        }
        other => panic!("{msg}(应为数据库约束错误,实际:{other:?})"),
    }
}

/// 插入一个 admin(只给必填列,其余走 DEFAULT),返回 id。
async fn insert_admin(pool: &PgPool, phone: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name) \
         VALUES ($1, $2, 'hash', 'name')",
    )
    .bind(id)
    .bind(phone)
    .execute(pool)
    .await
    .expect("插入 admin 应成功");
    id
}

/// 插入一枚 admin refresh token,expires_at 固定未来 30 天。
async fn insert_admin_token(
    pool: &PgPool,
    admin_id: Uuid,
    token_hash: &str,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO admin_refresh_tokens (id, admin_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, now() + interval '30 days')",
    )
    .bind(id)
    .bind(admin_id)
    .bind(token_hash)
    .execute(pool)
    .await?;
    Ok(id)
}

// ===== admins:默认值 =====

#[sqlx::test]
async fn admin_defaults_are_correct(pool: PgPool) {
    let id = insert_admin(&pool, "13800000001").await;

    let (level, status, must_change, failed_count, locked_null, email_null): (
        String,
        String,
        bool,
        i32,
        bool,
        bool,
    ) = sqlx::query_as(
        "SELECT level, status, must_change_password, failed_login_count, \
                locked_until IS NULL, email IS NULL \
         FROM admins WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("查询应成功");

    assert_eq!(level, "admin", "level 默认应为 admin(super 仅 seed 可造)");
    assert_eq!(status, "active", "status 默认应为 active");
    assert!(!must_change, "must_change_password 默认应为 false");
    assert_eq!(failed_count, 0, "failed_login_count 默认应为 0");
    assert!(locked_null, "locked_until 默认应为 NULL(未锁定)");
    assert!(email_null, "email 未给时应为 NULL(可选列)");
}

// ===== admins:唯一约束 =====

#[sqlx::test]
async fn admin_phone_must_be_unique(pool: PgPool) {
    insert_admin(&pool, "13800000001").await;
    let dup = sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name) \
         VALUES ($1, '13800000001', 'hash', 'other')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await;
    assert_db_error_code(dup, "23505", "重复 phone 应被唯一索引拒绝");
}

#[sqlx::test]
async fn admin_email_unique_is_case_insensitive(pool: PgPool) {
    sqlx::query(
        "INSERT INTO admins (id, phone, email, password_hash, display_name) \
         VALUES ($1, '13800000001', 'Admin@Example.com', 'hash', 'a')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .expect("首个 email 应成功");

    let dup = sqlx::query(
        "INSERT INTO admins (id, phone, email, password_hash, display_name) \
         VALUES ($1, '13800000002', 'admin@example.COM', 'hash', 'b')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await;
    assert_db_error_code(dup, "23505", "email 仅大小写不同也应被 lower(email) 唯一索引拒绝");
}

#[sqlx::test]
async fn multiple_admins_without_email_can_coexist(pool: PgPool) {
    // 部分唯一索引 WHERE email IS NOT NULL:NULL 行不参与唯一性。
    insert_admin(&pool, "13800000001").await;
    insert_admin(&pool, "13800000002").await;

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM admins WHERE email IS NULL")
        .fetch_one(&pool)
        .await
        .expect("查询应成功");
    assert_eq!(count, 2, "无 email 的 admin 应可并存(部分索引不拦 NULL)");
}

// ===== admins:CHECK 约束 =====

#[sqlx::test]
async fn admin_level_check_rejects_unknown_value(pool: PgPool) {
    let bad = sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name, level) \
         VALUES ($1, '13800000001', 'hash', 'a', 'root')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await;
    assert_db_error_code(bad, "23514", "level CHECK 应拒绝 admin/super_admin 以外的值");
}

#[sqlx::test]
async fn admin_level_accepts_super_admin(pool: PgPool) {
    sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name, level) \
         VALUES ($1, '13800000001', 'hash', 'a', 'super_admin')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .expect("level=super_admin 应被 CHECK 放行(seed 路径要用)");
}

#[sqlx::test]
async fn admin_status_check_rejects_unknown_value(pool: PgPool) {
    let bad = sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name, status) \
         VALUES ($1, '13800000001', 'hash', 'a', 'banned')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await;
    assert_db_error_code(bad, "23514", "status CHECK 应拒绝 active/disabled 以外的值");
}

// ===== admin_refresh_tokens =====

#[sqlx::test]
async fn admin_token_hash_must_be_unique(pool: PgPool) {
    let aid = insert_admin(&pool, "13800000001").await;
    insert_admin_token(&pool, aid, "same-hash")
        .await
        .expect("首枚 token 应成功");
    let dup = insert_admin_token(&pool, aid, "same-hash").await;
    assert_db_error_code(dup, "23505", "重复 token_hash 应被唯一索引拒绝");
}

#[sqlx::test]
async fn admin_token_for_nonexistent_admin_is_rejected(pool: PgPool) {
    let ghost = Uuid::now_v7();
    let bad = insert_admin_token(&pool, ghost, "h1").await;
    assert_db_error_code(bad, "23503", "给不存在的 admin 建 token 应被外键拒绝");
}

#[sqlx::test]
async fn deleting_admin_cascades_tokens(pool: PgPool) {
    let aid = insert_admin(&pool, "13800000001").await;
    insert_admin_token(&pool, aid, "h")
        .await
        .expect("建 token 应成功");

    sqlx::query("DELETE FROM admins WHERE id = $1")
        .bind(aid)
        .execute(&pool)
        .await
        .expect("删 admin 应成功");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM admin_refresh_tokens WHERE admin_id = $1")
            .bind(aid)
            .fetch_one(&pool)
            .await
            .expect("查询应成功");
    assert_eq!(count, 0, "删 admin 应级联删除其全部 token");
}

#[sqlx::test]
async fn admin_token_revoked_and_rotated_default_null(pool: PgPool) {
    let aid = insert_admin(&pool, "13800000001").await;
    let id = insert_admin_token(&pool, aid, "h")
        .await
        .expect("建 token 应成功");

    let (revoked_null, rotated_null): (bool, bool) = sqlx::query_as(
        "SELECT revoked_at IS NULL, rotated_at IS NULL FROM admin_refresh_tokens WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("查询应成功");

    assert!(revoked_null, "revoked_at 默认应为 NULL(未吊销)");
    assert!(rotated_null, "rotated_at 默认应为 NULL(未轮换)");
}

#[sqlx::test]
async fn admin_can_hold_multiple_tokens_at_schema_level(pool: PgPool) {
    // 「严格单登录」(Q1)是 service 层语义(issue 前 revoke_all),schema 刻意不拦——
    // 宽限窗口/重放验尸都需要历史行共存。此测试钉住这条边界,防止有人给表加错约束。
    let aid = insert_admin(&pool, "13800000001").await;
    insert_admin_token(&pool, aid, "hash-a")
        .await
        .expect("token A 应成功");
    insert_admin_token(&pool, aid, "hash-b")
        .await
        .expect("token B 应成功");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM admin_refresh_tokens WHERE admin_id = $1")
            .bind(aid)
            .fetch_one(&pool)
            .await
            .expect("查询应成功");
    assert_eq!(count, 2, "schema 层允许同 admin 多行 token(单登录归 service 管)");
}

// ===== 与 web 身份库的隔离边界 =====

#[sqlx::test]
async fn same_phone_can_exist_in_both_users_and_admins(pool: PgPool) {
    // 隔离铁律(§2):同一手机号既是学员又是管理员,两行互不相干、各自唯一。
    sqlx::query(
        "INSERT INTO users (id, phone, password_hash, display_name) \
         VALUES ($1, '13800000001', 'hash', 'web-user')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .expect("web 用户应成功");

    insert_admin(&pool, "13800000001").await; // 同号 admin 不应撞 users 的唯一约束
}

#[sqlx::test]
async fn same_token_hash_can_exist_in_both_session_tables(pool: PgPool) {
    // 两张会话表各自唯一索引、互不联动——admin 会话仓库绝不可能查到 web 行,反之亦然。
    let uid = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, phone, password_hash, display_name) \
         VALUES ($1, '13900000001', 'hash', 'web-user')",
    )
    .bind(uid)
    .execute(&pool)
    .await
    .expect("web 用户应成功");
    sqlx::query(
        "INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at) \
         VALUES ($1, $2, 'shared-hash', now() + interval '30 days')",
    )
    .bind(Uuid::now_v7())
    .bind(uid)
    .execute(&pool)
    .await
    .expect("web token 应成功");

    let aid = insert_admin(&pool, "13800000001").await;
    insert_admin_token(&pool, aid, "shared-hash")
        .await
        .expect("同哈希在 admin 表应可并存(两表唯一索引各自独立)");
}
