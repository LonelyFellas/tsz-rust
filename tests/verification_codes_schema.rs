//! verification_codes 表的 schema 约束测试。
//! 验证 channel/purpose 值域、默认值、以及「无外键」（可给未注册标识发码）。

use sqlx::PgPool;
use uuid::Uuid;

/// 插入一条验证码，成功返回其 id。expires_at 固定给未来 5 分钟。
async fn insert_code(
    pool: &PgPool,
    target: &str,
    channel: &str,
    purpose: &str,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO verification_codes (id, target, channel, purpose, code, expires_at) \
         VALUES ($1, $2, $3, $4, '123456', now() + interval '5 minutes')",
    )
    .bind(id)
    .bind(target)
    .bind(channel)
    .bind(purpose)
    .execute(pool)
    .await?;
    Ok(id)
}

#[sqlx::test]
async fn channel_rejects_unknown_value(pool: PgPool) {
    let bad = insert_code(&pool, "13800000000", "wechat", "login").await;
    assert!(bad.is_err(), "非法 channel 应被 CHECK 拒绝");
}

#[sqlx::test]
async fn purpose_rejects_unknown_value(pool: PgPool) {
    let bad = insert_code(&pool, "13800000000", "sms", "bogus").await;
    assert!(bad.is_err(), "非法 purpose 应被 CHECK 拒绝");
}

#[sqlx::test]
async fn all_four_purposes_accepted(pool: PgPool) {
    for purpose in [
        "login",
        "password_reset",
        "account_deletion",
        "contact_bind",
    ] {
        insert_code(&pool, "13800000000", "sms", purpose)
            .await
            .unwrap_or_else(|_| panic!("purpose={purpose} 应被接受"));
    }
}

#[sqlx::test]
async fn defaults_attempts_zero_and_unconsumed(pool: PgPool) {
    let id = insert_code(&pool, "13800000000", "sms", "login")
        .await
        .expect("插入应成功");

    let (attempts, consumed_is_null): (i32, bool) = sqlx::query_as(
        "SELECT attempts, consumed_at IS NULL FROM verification_codes WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("查询应成功");

    assert_eq!(attempts, 0, "attempts 默认应为 0");
    assert!(consumed_is_null, "consumed_at 默认应为 NULL（未消费）");
}

#[sqlx::test]
async fn code_for_unregistered_target_is_allowed(pool: PgPool) {
    // 无外键：给一个从未注册的手机号发码也应成功（注册前发码的场景）。
    insert_code(&pool, "19999999999", "sms", "login")
        .await
        .expect("给未注册标识发码应成功（该表不依赖 users）");
}
