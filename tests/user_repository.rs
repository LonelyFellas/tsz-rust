//! `UserRepository::create` 的行为测试。
//!
//! 用 `#[sqlx::test]`：每个测试拿到独立临时库、自动跑 `migrations/`、结束回滚，
//! 直接对真库验证（这也是唯一能验证枚举 ↔ TEXT 运行时往返、唯一冲突映射的地方——
//! 编译过 ≠ 运行对）。需要 `.env` 的 `DATABASE_URL` + 本地 Docker 库在跑。

use sqlx::PgPool;
use uuid::Uuid;

use tsz_rust::user::model::{UserRole, UserStatus};
use tsz_rust::user::repository::{NewUser, UserError, UserRepository};

/// 造一个测试用 NewUser。phone/email 传不同的值，能顺带网住「两个 Option<String>
/// 参数传反」——编译器抓不到这一种，就靠这里的断言。
fn new_user(phone: Option<&str>, email: Option<&str>, role: UserRole) -> NewUser {
    NewUser {
        id: Uuid::now_v7(),
        phone: phone.map(str::to_owned),
        email: email.map(str::to_owned),
        password_hash: "hashed-pw".to_owned(),
        display_name: "Alice".to_owned(),
        first_role: role,
        registration_ip: None,
    }
}

#[sqlx::test]
async fn create_persists_user_with_defaults_and_active_role(pool: PgPool) {
    let repo = UserRepository::new(pool.clone());
    // phone 与 email 取不同值：若 create 里两个 Option 参数传反，下面断言会立刻挂。
    let input = new_user(
        Some("13800138000"),
        Some("alice@example.com"),
        UserRole::Student,
    );
    let id = input.id;

    let user = repo.create(input).await.expect("create 应成功");

    // 身份字段原样保留
    assert_eq!(user.id, id);
    assert_eq!(user.phone.as_deref(), Some("13800138000"));
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
    assert_eq!(user.display_name, "Alice");
    assert_eq!(user.password_hash, "hashed-pw");
    // DB 默认值
    assert_eq!(user.status, UserStatus::Active, "status 应默认 active");
    assert_eq!(user.avatar_url, "", "avatar_url 应默认空串");
    // 首个角色即激活角色；能从 DB 正确回读 = 枚举 type_name/rename_all 映射对
    assert_eq!(user.last_active_role, Some(UserRole::Student));

    // 确认真落库了（另查一次）
    let name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("应能按 id 查回");
    assert_eq!(name, "Alice");
}

#[sqlx::test]
async fn create_records_first_role_in_user_roles(pool: PgPool) {
    let repo = UserRepository::new(pool.clone());
    let input = new_user(Some("13800138001"), None, UserRole::Teacher);
    let id = input.id;

    repo.create(input).await.expect("create 应成功");

    // user_roles 应记下该用户持有的首个角色（查回也验证了 user_roles.role 的枚举往返）。
    let roles: Vec<UserRole> = sqlx::query_scalar("SELECT role FROM user_roles WHERE user_id = $1")
        .bind(id)
        .fetch_all(&pool)
        .await
        .expect("查 user_roles 应成功");
    assert_eq!(
        roles,
        vec![UserRole::Teacher],
        "create 应把首个角色写进 user_roles"
    );
}

#[sqlx::test]
async fn create_rejects_duplicate_phone(pool: PgPool) {
    let repo = UserRepository::new(pool.clone());
    repo.create(new_user(Some("13900139000"), None, UserRole::Student))
        .await
        .expect("首个用户应成功");

    // 同手机号再来一次：唯一索引冲突应被映射成领域错误，而不是漏成 Db(...)。
    let dup = repo
        .create(new_user(Some("13900139000"), None, UserRole::Student))
        .await;
    assert!(
        matches!(dup, Err(UserError::PhoneNumberAlreadyExists)),
        "重复手机号应映射成 PhoneNumberAlreadyExists，实际: {dup:?}"
    );
}

#[sqlx::test]
async fn create_rejects_duplicate_email_case_insensitive(pool: PgPool) {
    let repo = UserRepository::new(pool.clone());
    repo.create(new_user(None, Some("Tom@Example.com"), UserRole::Student))
        .await
        .expect("首个用户应成功");

    // lower(email) 唯一索引：大小写不同的同一邮箱应冲突 → EmailAlreadyExists。
    let dup = repo
        .create(new_user(None, Some("tom@example.com"), UserRole::Student))
        .await;
    assert!(
        matches!(dup, Err(UserError::EmailAlreadyExists)),
        "大小写不同的同邮箱应映射成 EmailAlreadyExists，实际: {dup:?}"
    );
}

#[sqlx::test]
async fn get_by_identifier_by_phone_returns_the_matching_user(pool: PgPool) {
    let repo = UserRepository::new(pool.clone());
    let created = repo
        .create(new_user(
            Some("13800138000"),
            Some("alice@example.com"),
            UserRole::Student,
        ))
        .await
        .expect("create 应成功");

    // 传手机号：`WHERE phone=$1 OR email=$1` 应命中 phone 列。
    let got = repo
        .get_by_identifier("13800138000")
        .await
        .expect("应按手机查到该用户");

    assert_eq!(got.id, created.id);
    assert_eq!(got.phone.as_deref(), Some("13800138000"));
    assert_eq!(got.email.as_deref(), Some("alice@example.com"));
    assert_eq!(got.display_name, "Alice");
    assert_eq!(got.status, UserStatus::Active);
    assert_eq!(got.last_active_role, Some(UserRole::Student));
}

#[sqlx::test]
async fn get_by_identifier_unknown_is_not_found(pool: PgPool) {
    let repo = UserRepository::new(pool.clone());
    // 库里没有这个标识：应返回 NotFound，而不是漏成 Db(RowNotFound)。
    let result = repo.get_by_identifier("10000000000").await;
    assert!(
        matches!(result, Err(UserError::NotFound)),
        "查不到的标识应返回 NotFound，实际: {result:?}"
    );
}

#[sqlx::test]
async fn get_by_identifier_does_not_return_a_different_user(pool: PgPool) {
    let repo = UserRepository::new(pool.clone());
    let a = repo
        .create(new_user(Some("13800138000"), None, UserRole::Student))
        .await
        .expect("用户 A 应创建成功");
    repo.create(new_user(Some("13900139000"), None, UserRole::Teacher))
        .await
        .expect("用户 B 应创建成功");

    // 查 A 的号应精确返回 A，绝不串到 B。
    let got = repo
        .get_by_identifier("13800138000")
        .await
        .expect("应查到 A");
    assert_eq!(got.id, a.id, "应返回 A，不能串到 B");
    assert_eq!(got.last_active_role, Some(UserRole::Student));
}

/// 传邮箱应命中 email 列（`get_by_identifier` 的另一半能力）。
/// 注：repository 不做归一化，存/查都用小写邮箱（归一化是 service 的活）。
#[sqlx::test]
async fn get_by_identifier_by_email_returns_the_matching_user(pool: PgPool) {
    let repo = UserRepository::new(pool.clone());
    let created = repo
        .create(new_user(None, Some("bob@example.com"), UserRole::Student))
        .await
        .expect("create 应成功");

    let got = repo
        .get_by_identifier("bob@example.com")
        .await
        .expect("应按邮箱查到该用户");
    assert_eq!(got.id, created.id, "应按邮箱命中同一用户");
    assert_eq!(got.email.as_deref(), Some("bob@example.com"));
    assert_eq!(got.phone, None, "该用户无手机");
}

#[sqlx::test]
async fn nickname_update_is_local_and_allows_duplicates_and_retries(pool: PgPool) {
    let repo = UserRepository::new(pool.clone());
    let id = repo
        .create(new_user(
            Some("13800138000"),
            Some("alice@example.com"),
            UserRole::Teacher,
        ))
        .await
        .unwrap()
        .id;
    sqlx::query("UPDATE users SET avatar_url = 'avatar-before', security_version = 7, updated_at = created_at - INTERVAL '1 day' WHERE id = $1").bind(id).execute(&pool).await.unwrap();
    let before = repo.get_by_id(&id).await.unwrap();
    let roles = repo.get_roles_by_user_id(&id).await.unwrap();
    let other = repo
        .create(new_user(Some("13900139000"), None, UserRole::Student))
        .await
        .unwrap();
    for _ in 0..2 {
        let saved = repo
            .update_display_name(id, 7, &other.display_name)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.display_name, other.display_name);
        assert!(saved.updated_at > before.updated_at);
        assert_eq!(saved.id, before.id);
        assert_eq!(saved.phone, before.phone);
        assert_eq!(saved.email, before.email);
        assert_eq!(saved.avatar_url, before.avatar_url);
        assert_eq!(saved.password_hash, before.password_hash);
        assert_eq!(saved.security_version, before.security_version);
        assert_eq!(saved.last_active_role, before.last_active_role);
        assert_eq!(saved.status, before.status);
        assert_eq!(saved.created_at, before.created_at);
        assert_eq!(repo.get_roles_by_user_id(&id).await.unwrap(), roles);
    }
    assert!(
        repo.update_display_name(id, 6, "stale")
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        repo.update_display_name(id, 7, "disabled")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.update_display_name(Uuid::now_v7(), 0, "ghost")
            .await
            .unwrap()
            .is_none()
    );
}

async fn wait_for_blocked_nickname_update(pool: &PgPool) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE 'SELECT id FROM users WHERE id=$1%' AND query LIKE '%security_version=$2 FOR UPDATE')").fetch_one(pool).await.unwrap();
            if blocked { return; }
            tokio::task::yield_now().await;
        }
    }).await.expect("nickname write should wait on the user row lock");
}

#[sqlx::test]
async fn nickname_waits_for_avatar_and_preserves_other_fields(pool: PgPool) {
    let repo = UserRepository::new(pool.clone());
    let id = repo
        .create(new_user(Some("13800138000"), None, UserRole::Student))
        .await
        .unwrap()
        .id;
    let mut avatar = pool.begin().await.unwrap();
    sqlx::query("UPDATE users SET avatar_url = 'concurrent-avatar' WHERE id = $1")
        .bind(id)
        .execute(&mut *avatar)
        .await
        .unwrap();
    let task = tokio::spawn(async move {
        repo.update_display_name(id, 0, "concurrent-name")
            .await
            .unwrap()
    });
    wait_for_blocked_nickname_update(&pool).await;
    avatar.commit().await.unwrap();
    let saved = task.await.unwrap().unwrap();
    assert_eq!(saved.avatar_url, "concurrent-avatar");
    assert_eq!(saved.display_name, "concurrent-name");
    sqlx::query("UPDATE users SET avatar_url = 'later-avatar' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let saved = UserRepository::new(pool).get_by_id(&id).await.unwrap();
    assert_eq!(saved.avatar_url, "later-avatar");
    assert_eq!(saved.display_name, "concurrent-name");
}

#[sqlx::test]
async fn nickname_and_admin_updates_follow_successful_write_order(pool: PgPool) {
    let repo = UserRepository::new(pool.clone());
    let id = repo
        .create(new_user(Some("13800138000"), None, UserRole::Student))
        .await
        .unwrap()
        .id;
    let mut admin = pool.begin().await.unwrap();
    sqlx::query("UPDATE users SET display_name = 'admin-name' WHERE id = $1")
        .bind(id)
        .execute(&mut *admin)
        .await
        .unwrap();
    let task =
        tokio::spawn(async move { repo.update_display_name(id, 0, "self-name").await.unwrap() });
    wait_for_blocked_nickname_update(&pool).await;
    admin.commit().await.unwrap();
    assert_eq!(task.await.unwrap().unwrap().display_name, "self-name");
    sqlx::query("UPDATE users SET display_name = 'admin-later' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        UserRepository::new(pool)
            .get_by_id(&id)
            .await
            .unwrap()
            .display_name,
        "admin-later"
    );
}

#[sqlx::test]
async fn nickname_rechecks_predicate_after_concurrent_security_or_deletion(pool: PgPool) {
    for mutation in [
        "UPDATE users SET status = 'disabled' WHERE id = $1",
        "UPDATE users SET security_version = security_version + 1 WHERE id = $1",
        "DELETE FROM users WHERE id = $1",
    ] {
        let repo = UserRepository::new(pool.clone());
        let id = repo
            .create(new_user(Some("13800138000"), None, UserRole::Student))
            .await
            .unwrap()
            .id;
        let mut security = pool.begin().await.unwrap();
        sqlx::query(mutation)
            .bind(id)
            .execute(&mut *security)
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            repo.update_display_name(id, 0, "not-allowed")
                .await
                .unwrap()
        });
        wait_for_blocked_nickname_update(&pool).await;
        security.commit().await.unwrap();
        assert!(task.await.unwrap().is_none());
        let name = sqlx::query_scalar::<_, String>("SELECT display_name FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(name.is_none() || name.as_deref() == Some("Alice"));
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
