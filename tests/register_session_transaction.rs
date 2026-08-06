use chrono::{Duration, Utc};
use sqlx::PgPool;
use tsz_rust::{
    session::{model::NewRefreshToken, repository::RefreshTokenRepository},
    user::{
        model::UserRole,
        repository::{NewUser, UserRepository},
    },
};
use uuid::Uuid;

#[sqlx::test]
async fn refresh_insert_failure_rolls_back_new_user(pool: PgPool) {
    let phone = "13800138000";
    let mut tx = pool.begin().await.unwrap();
    let user = UserRepository::create_in(
        &mut tx,
        NewUser {
            id: Uuid::now_v7(),
            phone: Some(phone.to_owned()),
            email: None,
            password_hash: "test-hash".to_owned(),
            display_name: "测试用户".to_owned(),
            first_role: UserRole::Student,
        },
    )
    .await
    .unwrap();

    let error = RefreshTokenRepository::insert_in(
        &mut tx,
        NewRefreshToken {
            id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            token_hash: "forced-refresh-insert-failure".to_owned(),
            expires_at: Utc::now() + Duration::days(30),
        },
    )
    .await
    .expect_err("不存在的 user_id 应触发 refresh token 外键错误");
    assert!(matches!(
        error,
        tsz_rust::session::repository::RefreshTokenError::Db(_)
    ));
    drop(tx);

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "refresh 写入失败后不得残留已创建用户");
}
