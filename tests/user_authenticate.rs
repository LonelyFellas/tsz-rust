//! `UserService::authenticate` 的行为测试（真库，`#[sqlx::test]`）。
//!
//! 用 `register` 先造真实用户（密码经 bcrypt），再验 `authenticate` 的凭证校验。
//! 重点钉住三条安全契约：
//!   1. 用户不存在 与 密码错 → **同一个** `InvalidCredentials`（不可区分）；
//!   2. 先验密码、再查状态：`AccountDisabled` 只在密码正确后才可能出现；
//!   3. 手机 / 邮箱两种 identifier 都能登，邮箱大小写不敏感。

use sqlx::PgPool;
use uuid::Uuid;

use tsz_rust::user::repository::UserRepository;
use tsz_rust::user::service::{LoginError, RegisterInput, UserService};

fn service(pool: PgPool) -> UserService {
    UserService::new(UserRepository::new(pool))
}

/// 造一个已注册用户，返回其 id。密码固定 "password123"。
async fn register_user(svc: &UserService, phone: Option<&str>, email: Option<&str>) -> Uuid {
    svc.register(RegisterInput {
        phone: phone.map(str::to_owned),
        email: email.map(str::to_owned),
        password: "password123".to_owned(),
    })
    .await
    .expect("注册应成功")
    .id
}

// ————————————————————— 成功路径 —————————————————————

#[sqlx::test]
async fn authenticate_succeeds_with_phone(pool: PgPool) {
    let svc = service(pool);
    let id = register_user(&svc, Some("13800138000"), None).await;

    let user = svc
        .authenticate("13800138000", "password123")
        .await
        .expect("正确手机号+密码应登录成功");
    assert_eq!(user.id, id);
}

#[sqlx::test]
async fn authenticate_succeeds_with_email_case_insensitive(pool: PgPool) {
    let svc = service(pool);
    let id = register_user(&svc, None, Some("alice@example.com")).await;

    // 登录用大写邮箱：normalize_identifier 会小写化，应命中入库的小写邮箱。
    let user = svc
        .authenticate("Alice@Example.com", "password123")
        .await
        .expect("邮箱应大小写不敏感");
    assert_eq!(user.id, id);
}

// ————————————————————— 不可区分：两种失败同一个错 —————————————————————

#[sqlx::test]
async fn wrong_password_is_invalid_credentials(pool: PgPool) {
    let svc = service(pool);
    register_user(&svc, Some("13800138000"), None).await;

    let err = svc
        .authenticate("13800138000", "wrong-password")
        .await
        .expect_err("密码错应失败");
    assert!(
        matches!(err, LoginError::InvalidCredentials),
        "密码错应是 InvalidCredentials，实际 {err:?}"
    );
}

#[sqlx::test]
async fn unknown_identifier_is_invalid_credentials(pool: PgPool) {
    let svc = service(pool);
    // 库里没有这个号：必须返回【和密码错完全相同】的错误，不能泄露「查无此人」。
    let err = svc
        .authenticate("19999999999", "password123")
        .await
        .expect_err("未知用户应失败");
    assert!(
        matches!(err, LoginError::InvalidCredentials),
        "未知 identifier 应是 InvalidCredentials（和密码错不可区分），实际 {err:?}"
    );
}

// ————————————————————— 先验密码、再查状态 —————————————————————

#[sqlx::test]
async fn disabled_account_with_correct_password_is_account_disabled(pool: PgPool) {
    let svc = service(pool.clone());
    let id = register_user(&svc, Some("13800138000"), None).await;

    // 禁用该账号。
    sqlx::query!("UPDATE users SET status = 'disabled' WHERE id = $1", id)
        .execute(&pool)
        .await
        .unwrap();

    // 密码【正确】→ 过了密码校验 → 才暴露 AccountDisabled。
    let err = svc
        .authenticate("13800138000", "password123")
        .await
        .expect_err("禁用账号应失败");
    assert!(
        matches!(err, LoginError::AccountDisabled),
        "密码对+账号禁用应是 AccountDisabled，实际 {err:?}"
    );
}

#[sqlx::test]
async fn disabled_account_with_wrong_password_still_invalid_credentials(pool: PgPool) {
    let svc = service(pool.clone());
    let id = register_user(&svc, Some("13800138000"), None).await;
    sqlx::query!("UPDATE users SET status = 'disabled' WHERE id = $1", id)
        .execute(&pool)
        .await
        .unwrap();

    // 密码【错】→ 卡在密码校验，看不到 AccountDisabled（禁用状态不泄露给没密码的人）。
    let err = svc
        .authenticate("13800138000", "wrong-password")
        .await
        .expect_err("应失败");
    assert!(
        matches!(err, LoginError::InvalidCredentials),
        "密码错时即便账号被禁也应只报 InvalidCredentials，实际 {err:?}"
    );
}
