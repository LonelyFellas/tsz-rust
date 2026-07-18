//! student_profiles 表的 schema 约束测试。
//!
//! 用 `#[sqlx::test]`：每个测试独立临时库、自动跑迁移、结束回滚。
//! 重点验证「学习设置全有或全无」这条 DB 级约束真的生效。

use sqlx::PgPool;
use uuid::Uuid;

/// 插入一个合法用户，返回 id（用 email 满足二选一，email 由 uuid 派生保证唯一）。
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

/// 插入学生资料，学习设置传 None 即写 NULL。返回 Result 便于断言成功 / 失败。
async fn insert_profile(
    pool: &PgPool,
    user_id: Uuid,
    cefr: Option<&str>,
    variant: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO student_profiles (user_id, cefr_level, english_variant) \
         VALUES ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(cefr)
    .bind(variant)
    .execute(pool)
    .await
    .map(|_| ())
}

// —— 学习设置「全有或全无」——

#[sqlx::test]
async fn learning_settings_both_null_is_allowed(pool: PgPool) {
    let uid = insert_user(&pool).await;
    insert_profile(&pool, uid, None, None)
        .await
        .expect("两个学习设置都不设应放行（尚未 onboard）");
}

#[sqlx::test]
async fn learning_settings_both_set_is_allowed(pool: PgPool) {
    let uid = insert_user(&pool).await;
    insert_profile(&pool, uid, Some("B1"), Some("BrE"))
        .await
        .expect("两个学习设置都设应放行");
}

#[sqlx::test]
async fn learning_settings_only_cefr_is_rejected(pool: PgPool) {
    let uid = insert_user(&pool).await;
    let half = insert_profile(&pool, uid, Some("B1"), None).await;
    assert!(
        half.is_err(),
        "只设 cefr_level、不设 english_variant 应被成对约束拒绝"
    );
}

#[sqlx::test]
async fn learning_settings_only_variant_is_rejected(pool: PgPool) {
    let uid = insert_user(&pool).await;
    let half = insert_profile(&pool, uid, None, Some("AmE")).await;
    assert!(
        half.is_err(),
        "只设 english_variant、不设 cefr_level 应被成对约束拒绝"
    );
}

// —— 各自的值域 CHECK ——

#[sqlx::test]
async fn cefr_level_rejects_unknown_value(pool: PgPool) {
    let uid = insert_user(&pool).await;
    // variant 给合法值，隔离出 cefr 的 CHECK 作为唯一拒绝原因。
    let bad = insert_profile(&pool, uid, Some("Z9"), Some("BrE")).await;
    assert!(bad.is_err(), "非法 cefr_level 应被 CHECK 拒绝");
}

#[sqlx::test]
async fn english_variant_rejects_unknown_value(pool: PgPool) {
    let uid = insert_user(&pool).await;
    let bad = insert_profile(&pool, uid, Some("A1"), Some("XX")).await;
    assert!(bad.is_err(), "非法 english_variant 应被 CHECK 拒绝");
}

// —— 主键 / 外键 / 级联 ——

#[sqlx::test]
async fn one_profile_per_user(pool: PgPool) {
    let uid = insert_user(&pool).await;
    insert_profile(&pool, uid, None, None)
        .await
        .expect("首份资料应成功");
    let dup = insert_profile(&pool, uid, None, None).await;
    assert!(dup.is_err(), "同一用户第二份资料应被主键拒绝（1:1）");
}

#[sqlx::test]
async fn profile_for_nonexistent_user_is_rejected(pool: PgPool) {
    let ghost = Uuid::now_v7(); // 从未插入 users
    let bad = insert_profile(&pool, ghost, None, None).await;
    assert!(bad.is_err(), "给不存在的用户建资料应被外键拒绝");
}

#[sqlx::test]
async fn deleting_user_cascades_profile(pool: PgPool) {
    let uid = insert_user(&pool).await;
    insert_profile(&pool, uid, None, None)
        .await
        .expect("建资料应成功");

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(uid)
        .execute(&pool)
        .await
        .expect("删用户应成功");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM student_profiles WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await
        .expect("查询应成功");
    assert_eq!(count, 0, "删用户应级联删除其资料");
}

#[sqlx::test]
async fn updated_at_trigger_bumps_on_update(pool: PgPool) {
    let uid = insert_user(&pool).await;
    // 故意把 updated_at 塞成 2000 年；触发器不生效它就停在 2000。
    sqlx::query(
        "INSERT INTO student_profiles (user_id, updated_at) \
         VALUES ($1, '2000-01-01T00:00:00Z')",
    )
    .bind(uid)
    .execute(&pool)
    .await
    .expect("插入应成功");

    sqlx::query("UPDATE student_profiles SET grade = '初一' WHERE user_id = $1")
        .bind(uid)
        .execute(&pool)
        .await
        .expect("更新应成功");

    let bumped: bool = sqlx::query_scalar(
        "SELECT updated_at > created_at FROM student_profiles WHERE user_id = $1",
    )
    .bind(uid)
    .fetch_one(&pool)
    .await
    .expect("查询应成功");
    assert!(bumped, "UPDATE 后触发器应把 updated_at 刷新为当前时间");
}
