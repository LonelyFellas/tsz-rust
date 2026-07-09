//! teacher_profiles 表的 schema 约束测试。
//! 结构是 student_profiles 的简化镜像（无学习设置），验证主键 1:1、外键 CASCADE、默认值、触发器。

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

/// 只给 user_id 建教师资料（bio/verified 用默认值）。返回 Result 便于断言。
async fn insert_teacher(pool: &PgPool, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO teacher_profiles (user_id) VALUES ($1)")
        .bind(user_id)
        .execute(pool)
        .await
        .map(|_| ())
}

#[sqlx::test]
async fn one_profile_per_user(pool: PgPool) {
    let uid = insert_user(&pool).await;
    insert_teacher(&pool, uid).await.expect("首份资料应成功");
    let dup = insert_teacher(&pool, uid).await;
    assert!(dup.is_err(), "同一用户第二份资料应被主键拒绝（1:1）");
}

#[sqlx::test]
async fn profile_for_nonexistent_user_is_rejected(pool: PgPool) {
    let ghost = Uuid::now_v7();
    let bad = insert_teacher(&pool, ghost).await;
    assert!(bad.is_err(), "给不存在的用户建资料应被外键拒绝");
}

#[sqlx::test]
async fn deleting_user_cascades_profile(pool: PgPool) {
    let uid = insert_user(&pool).await;
    insert_teacher(&pool, uid).await.expect("建资料应成功");

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(uid)
        .execute(&pool)
        .await
        .expect("删用户应成功");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM teacher_profiles WHERE user_id = $1")
            .bind(uid)
            .fetch_one(&pool)
            .await
            .expect("查询应成功");
    assert_eq!(count, 0, "删用户应级联删除其资料");
}

#[sqlx::test]
async fn defaults_bio_empty_and_unverified(pool: PgPool) {
    let uid = insert_user(&pool).await;
    insert_teacher(&pool, uid).await.expect("建资料应成功");

    let (bio, verified): (String, bool) =
        sqlx::query_as("SELECT bio, verified FROM teacher_profiles WHERE user_id = $1")
            .bind(uid)
            .fetch_one(&pool)
            .await
            .expect("查询应成功");
    assert_eq!(bio, "", "bio 默认应为空串");
    assert!(!verified, "verified 默认应为 false");
}

#[sqlx::test]
async fn updated_at_trigger_bumps_on_update(pool: PgPool) {
    let uid = insert_user(&pool).await;
    sqlx::query(
        "INSERT INTO teacher_profiles (user_id, updated_at) \
         VALUES ($1, '2000-01-01T00:00:00Z')",
    )
    .bind(uid)
    .execute(&pool)
    .await
    .expect("插入应成功");

    sqlx::query("UPDATE teacher_profiles SET bio = '资深教师' WHERE user_id = $1")
        .bind(uid)
        .execute(&pool)
        .await
        .expect("更新应成功");

    let bumped: bool = sqlx::query_scalar(
        "SELECT updated_at > created_at FROM teacher_profiles WHERE user_id = $1",
    )
    .bind(uid)
    .fetch_one(&pool)
    .await
    .expect("查询应成功");
    assert!(bumped, "UPDATE 后触发器应把 updated_at 刷新为当前时间");
}
