//! refresh_tokens 表的 schema 约束测试。
//! 验证 token_hash 唯一、外键 CASCADE、多会话、revoked/rotated 默认值。

use sqlx::PgPool;
use uuid::Uuid;

async fn insert_user(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, email, password_hash, display_name) \
         VALUES ($1, $2, 'hash', 'name')",
    )
    .bind(id)
    .bind(format!("{}@x.com", id.simple()))
    .execute(pool)
    .await
    .expect("插入用户应成功");
    id
}

/// 插入一个 refresh token，成功返回其 id。expires_at 固定给未来 30 天。
async fn insert_token(pool: &PgPool, user_id: Uuid, token_hash: &str) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, now() + interval '30 days')",
    )
    .bind(id)
    .bind(user_id)
    .bind(token_hash)
    .execute(pool)
    .await?;
    Ok(id)
}

#[sqlx::test]
async fn token_hash_must_be_unique(pool: PgPool) {
    let uid = insert_user(&pool).await;
    insert_token(&pool, uid, "same-hash")
        .await
        .expect("首个 token 应成功");
    let dup = insert_token(&pool, uid, "same-hash").await;
    assert!(dup.is_err(), "重复 token_hash 应被唯一索引拒绝");
}

#[sqlx::test]
async fn token_for_nonexistent_user_is_rejected(pool: PgPool) {
    let ghost = Uuid::now_v7();
    let bad = insert_token(&pool, ghost, "h1").await;
    assert!(bad.is_err(), "给不存在的用户建 token 应被外键拒绝");
}

#[sqlx::test]
async fn user_can_have_multiple_tokens(pool: PgPool) {
    let uid = insert_user(&pool).await;
    insert_token(&pool, uid, "hash-a")
        .await
        .expect("token A 应成功");
    insert_token(&pool, uid, "hash-b")
        .await
        .expect("token B 应成功");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM refresh_tokens WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await
        .expect("查询应成功");
    assert_eq!(count, 2, "同一用户可持有多个 token（不同哈希）");
}

#[sqlx::test]
async fn deleting_user_cascades_tokens(pool: PgPool) {
    let uid = insert_user(&pool).await;
    insert_token(&pool, uid, "h")
        .await
        .expect("建 token 应成功");

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(uid)
        .execute(&pool)
        .await
        .expect("删用户应成功");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM refresh_tokens WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await
        .expect("查询应成功");
    assert_eq!(count, 0, "删用户应级联删除其全部 token");
}

#[sqlx::test]
async fn revoked_and_rotated_default_null(pool: PgPool) {
    let uid = insert_user(&pool).await;
    let id = insert_token(&pool, uid, "h")
        .await
        .expect("建 token 应成功");

    let (revoked_null, rotated_null): (bool, bool) = sqlx::query_as(
        "SELECT revoked_at IS NULL, rotated_at IS NULL FROM refresh_tokens WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("查询应成功");

    assert!(revoked_null, "revoked_at 默认应为 NULL（未吊销）");
    assert!(rotated_null, "rotated_at 默认应为 NULL（未轮换）");
}
