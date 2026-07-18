//! user_roles 表的 schema 约束测试。
//!
//! 用 `#[sqlx::test]`：每个测试独立临时库、自动跑迁移、结束回滚。
//! 验证「约束真的按预期拦截 / 放行」，不只是能建表。

use sqlx::PgPool;
use uuid::Uuid;

/// 插入一个合法用户，返回其 id。用 email 满足二选一约束，email 由 uuid 派生保证唯一。
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

/// 给用户加一个角色，返回 Result 便于断言成功 / 失败。
async fn add_role(pool: &PgPool, user_id: Uuid, role: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, $2)")
        .bind(user_id)
        .bind(role)
        .execute(pool)
        .await
        .map(|_| ())
}

#[sqlx::test]
async fn role_rejects_unknown_value(pool: PgPool) {
    // role 的 CHECK：只允许 student / teacher。
    let uid = insert_user(&pool).await;
    let bad = add_role(&pool, uid, "admin").await;
    assert!(bad.is_err(), "非法角色值应被 CHECK 拒绝");
}

#[sqlx::test]
async fn duplicate_user_role_is_rejected(pool: PgPool) {
    // 复合主键 (user_id, role)：同一用户同一角色不可重复。
    let uid = insert_user(&pool).await;
    add_role(&pool, uid, "student")
        .await
        .expect("首次加 student 应成功");
    let dup = add_role(&pool, uid, "student").await;
    assert!(dup.is_err(), "重复的 (user_id, role) 应被复合主键拒绝");
}

#[sqlx::test]
async fn role_for_nonexistent_user_is_rejected(pool: PgPool) {
    // 外键：user_id 必须指向真实存在的用户。
    let ghost = Uuid::now_v7(); // 从未插入 users
    let bad = add_role(&pool, ghost, "student").await;
    assert!(bad.is_err(), "给不存在的用户加角色应被外键拒绝");
}

#[sqlx::test]
async fn user_can_hold_both_roles(pool: PgPool) {
    // 一个账号可同时是 student + teacher（参照§2 的核心需求）。
    let uid = insert_user(&pool).await;
    add_role(&pool, uid, "student")
        .await
        .expect("student 应成功");
    add_role(&pool, uid, "teacher")
        .await
        .expect("teacher 应成功");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM user_roles WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await
        .expect("查询应成功");
    assert_eq!(count, 2, "一个用户应能同时持有两个角色");
}

#[sqlx::test]
async fn deleting_user_cascades_roles(pool: PgPool) {
    // ON DELETE CASCADE：删用户应自动清掉他的角色行，不留孤儿。
    let uid = insert_user(&pool).await;
    add_role(&pool, uid, "student").await.expect("加角色应成功");

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(uid)
        .execute(&pool)
        .await
        .expect("删用户应成功");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM user_roles WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await
        .expect("查询应成功");
    assert_eq!(count, 0, "删用户应级联删除其全部角色行");
}
