//! users 表的 schema 约束测试。
//!
//! 用 `#[sqlx::test]`：每个测试拿到一个独立临时库、自动跑完 `migrations/`，
//! 结束回滚。验证的不是「能建表」，而是「约束真的按预期拦截 / 放行」。
//! 需要 `.env` 里的 `DATABASE_URL` 指向本地库（Docker 需在跑）。

use sqlx::PgPool;
use uuid::Uuid;

/// 插入一个最小合法用户，成功返回其 v7 id。phone/email 传 None 即写入 NULL。
/// 其余 NOT NULL 列（status/avatar_url/时间戳）都有默认值，无需给。
async fn insert_user(
    pool: &PgPool,
    phone: Option<&str>,
    email: Option<&str>,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7(); // 应用层生成 v7 主键
    sqlx::query(
        "INSERT INTO users (id, phone, email, password_hash, display_name) \
         VALUES ($1, $2, $3, 'hash', 'name')",
    )
    .bind(id)
    .bind(phone)
    .bind(email)
    .execute(pool)
    .await?;
    Ok(id)
}

#[sqlx::test]
async fn rejects_row_with_neither_phone_nor_email(pool: PgPool) {
    // 二选一：两者皆空应被 users_phone_or_email_present 拒绝。
    let result = insert_user(&pool, None, None).await;
    assert!(result.is_err(), "phone 和 email 都为空应被 CHECK 拒绝");
}

#[sqlx::test]
async fn allows_multiple_rows_without_phone(pool: PgPool) {
    // 部分唯一索引：多行 phone 为 NULL 不算冲突（靠 email 满足二选一）。
    insert_user(&pool, None, Some("a@x.com"))
        .await
        .expect("第一行应插入成功");
    insert_user(&pool, None, Some("b@x.com"))
        .await
        .expect("第二行也应插入成功（NULL phone 不冲突）");
}

#[sqlx::test]
async fn phone_uniqueness_enforced(pool: PgPool) {
    insert_user(&pool, Some("13800000000"), None)
        .await
        .expect("首个手机号应插入成功");
    let dup = insert_user(&pool, Some("13800000000"), None).await;
    assert!(dup.is_err(), "重复手机号应被唯一索引拒绝");
}

#[sqlx::test]
async fn email_uniqueness_is_case_insensitive(pool: PgPool) {
    insert_user(&pool, None, Some("Tom@x.com"))
        .await
        .expect("首个邮箱应插入成功");
    // lower(email) 唯一索引：大小写不同但同一邮箱应冲突。
    let dup = insert_user(&pool, None, Some("tom@x.com")).await;
    assert!(dup.is_err(), "大小写不同的同一邮箱应被拒绝");
}

#[sqlx::test]
async fn status_rejects_unknown_value(pool: PgPool) {
    // status 的 CHECK：只允许 active / disabled。
    let id = Uuid::now_v7();
    let bad = sqlx::query(
        "INSERT INTO users (id, phone, password_hash, display_name, status) \
         VALUES ($1, '13900000000', 'h', 'n', 'bogus')",
    )
    .bind(id)
    .execute(&pool)
    .await;
    assert!(bad.is_err(), "非法 status 值应被 CHECK 拒绝");
}

#[sqlx::test]
async fn v7_uuid_row_can_be_read_back(pool: PgPool) {
    let id = insert_user(&pool, Some("13700000000"), None)
        .await
        .expect("v7 主键应能插入");

    let got: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("应能按 id 查回");
    assert_eq!(got, id);
}

#[sqlx::test]
async fn updated_at_trigger_bumps_on_update(pool: PgPool) {
    // 故意把 updated_at 塞成 2000 年：若触发器不生效，它会一直停在 2000。
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, phone, password_hash, display_name, updated_at) \
         VALUES ($1, '13600000000', 'h', 'n', '2000-01-01T00:00:00Z')",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect("插入应成功");

    sqlx::query("UPDATE users SET display_name = 'changed' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("更新应成功");

    // 触发器应把 updated_at 刷成 now()，于是它 > created_at（≈插入时刻，远晚于 2000）。
    // 若触发器没生效，updated_at 停在 2000 < created_at，断言会失败。
    let bumped: bool =
        sqlx::query_scalar("SELECT updated_at > created_at FROM users WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("查询应成功");
    assert!(bumped, "UPDATE 后触发器应把 updated_at 刷新为当前时间");
}
