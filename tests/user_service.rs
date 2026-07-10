//! `UserService::register` 的行为测试（真库，`#[sqlx::test]`）。
//!
//! 沿用「不搞 trait/fake」的决定：register 真调 `UserRepository::create`，所以
//! 整条走真库；断言的是**业务行为 + 「约束冲突→领域错误」的映射**，不重测裸 DB
//! 约束（那些在 `tests/*_schema.rs`）。来源：user-service-test-checklist §A + §0。
//!
//! ⚠️ 本文件对齐的 register 契约：
//!
//! ```ignore
//! pub struct RegisterInput {
//!     pub phone: Option<String>,      // raw，未 trim
//!     pub email: Option<String>,      // raw，未 trim/小写
//!     pub password: String,           // 明文，≥8 由 handler binding 保证
//! }
//! // 产品规则：注册【不收】display_name（后端随机生成默认昵称）；也【不收】role——
//! // 注册永远是 student，老师须在系统内申请。也【不含】code——验证码属独立 OTP 域。
//!
//! pub enum RegisterError {                       // 聚合错误：嵌套各子错误
//!     Register(SubjectError),    // PhoneOrEmailMissing / UserAlreadyExists（映射自 23505）
//!     Password(PasswordError),   // TooLong（>72 字节）等
//!     Repository(UserError),     // 其余底层错误如实透传（不谎报）
//! }
//!
//! pub struct UserService { /* 持 UserRepository */ }
//! impl UserService {
//!     pub fn new(repo: UserRepository) -> Self;
//!     pub async fn register(&self, input: RegisterInput) -> Result<User, RegisterError>;
//! }
//! ```
//!
//! register 内部要点：normalize_phone/normalize_email 后，**空串归一化为 None**
//! （DB 用 NULL 表示「无此标识」）；两者都为 None → Register(PhoneOrEmailMissing)。

use sqlx::PgPool;

use tsz_rust::user::model::{PasswordError, SubjectError, UserRole, UserStatus};
use tsz_rust::user::repository::UserRepository;
use tsz_rust::user::service::{DisplayName, RegisterError, RegisterInput, UserService};

fn service(pool: PgPool) -> UserService {
    UserService::new(UserRepository::new(pool))
}

/// 造一份合法输入。phone/email 传不同值，能顺带网住「两个 Option 参数传反」。
/// 注意：没有 role 参数（注册永远 student）、也没有 code（验证码属 OTP 域）。
fn valid_input(phone: Option<&str>, email: Option<&str>) -> RegisterInput {
    RegisterInput {
        phone: phone.map(str::to_owned),
        email: email.map(str::to_owned),
        password: "password123".to_owned(),
    }
}

// ————————————————————— 成功主线 —————————————————————

#[sqlx::test]
async fn register_persists_user_with_role_and_hashed_password(pool: PgPool) {
    let svc = service(pool.clone());
    let user = svc
        .register(valid_input(Some("13800138000"), Some("alice@example.com")))
        .await
        .expect("合法输入应注册成功");

    // 身份字段
    assert_eq!(user.phone.as_deref(), Some("13800138000"));
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
    // display_name 注册时后端随机生成（形容词+名词+数字）：不断言确切值，
    // 只断言性质——非空、能过 DisplayName 规则、且不泄露 subject（手机/邮箱）。
    assert!(!user.display_name.is_empty(), "默认昵称不应为空");
    assert!(
        DisplayName::parse(&user.display_name).is_ok(),
        "生成的默认昵称应满足 DisplayName 规则：{}",
        user.display_name
    );
    assert!(
        !user.display_name.contains("alice@example.com"),
        "默认昵称不应泄露邮箱：{}",
        user.display_name
    );
    // DB 默认
    assert_eq!(user.status, UserStatus::Active);
    // 注册永远是 student（老师须系统内申请，注册无从选 role）
    assert_eq!(user.last_active_role, Some(UserRole::Student));

    // 密码：绝不明文存储，且能 bcrypt 验证回原文
    assert_ne!(
        user.password_hash, "password123",
        "password_hash 不能是明文"
    );
    assert!(!user.password_hash.is_empty(), "password_hash 不能为空");
    assert!(
        bcrypt::verify("password123", &user.password_hash).expect("hash 应可被 bcrypt 解析"),
        "存的哈希应能通过 bcrypt::verify 验回原密码"
    );

    // 真落库了（另查一次），且 user_roles 记下了 student 角色
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "用户应已落库");

    let roles: Vec<UserRole> = sqlx::query_scalar("SELECT role FROM user_roles WHERE user_id = $1")
        .bind(user.id)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(roles, vec![UserRole::Student], "user_roles 应记下 student");

    // TODO(session 域)：断言返回体含 access token(subject=user.id, role=student)
    //                   + 非空 refresh token。token 签发接入后补。
}

/// 单手机账号合法（users_phone_or_email_present 只要求至少一个）。
#[sqlx::test]
async fn register_phone_only_succeeds(pool: PgPool) {
    let svc = service(pool);
    let user = svc
        .register(valid_input(Some("13800138001"), None))
        .await
        .expect("仅手机应注册成功");

    assert_eq!(user.phone.as_deref(), Some("13800138001"));
    assert_eq!(user.email, None, "未提供邮箱应存为 None，不是空串");
    assert_eq!(user.last_active_role, Some(UserRole::Student));
}

/// 单邮箱账号合法；邮箱应被小写化。
#[sqlx::test]
async fn register_email_only_succeeds(pool: PgPool) {
    let svc = service(pool);
    let user = svc
        .register(valid_input(None, Some("Bob@Example.com")))
        .await
        .expect("仅邮箱应注册成功");

    assert_eq!(user.phone, None, "未提供手机应存为 None");
    assert_eq!(
        user.email.as_deref(),
        Some("bob@example.com"),
        "邮箱应被小写化（对齐 lower(email) 唯一索引）"
    );
}

// ————————————————————— 归一化 —————————————————————

/// 手机号两端空白应被 trim 后再存。
#[sqlx::test]
async fn register_trims_phone(pool: PgPool) {
    let svc = service(pool);
    let input = valid_input(Some(" 13800138000 "), None);

    let user = svc.register(input).await.expect("应注册成功");
    assert_eq!(
        user.phone.as_deref(),
        Some("13800138000"),
        "手机号应被 trim"
    );
}

/// 混合大小写邮箱应被小写化后存储。
#[sqlx::test]
async fn register_lowercases_email(pool: PgPool) {
    let svc = service(pool);
    let user = svc
        .register(valid_input(None, Some("Alice@Example.COM")))
        .await
        .expect("应注册成功");
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
}

// ————————————————————— 校验早返回（不落库）—————————————————————

/// phone 与 email 归一化后都为空（含纯空白）→ Register(PhoneOrEmailMissing)，且不创建任何用户。
#[sqlx::test]
async fn register_missing_identifier_is_rejected(pool: PgPool) {
    let svc = service(pool.clone());

    // 二者皆 None
    let err = svc
        .register(valid_input(None, None))
        .await
        .expect_err("都无标识应失败");
    assert!(
        matches!(
            err,
            RegisterError::Register(SubjectError::PhoneOrEmailMissing)
        ),
        "实际：{err:?}"
    );

    // 纯空白 → 归一化为空 → 同样 PhoneOrEmailMissing（不能靠 DB 兜底）
    let err = svc
        .register(valid_input(Some("   "), Some("  ")))
        .await
        .expect_err("纯空白标识应失败");
    assert!(
        matches!(
            err,
            RegisterError::Register(SubjectError::PhoneOrEmailMissing)
        ),
        "实际：{err:?}"
    );

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "校验失败不应落任何用户");
}

/// 明文密码 > 72 字节（bcrypt 上限）→ Password(TooLong)，且不落半个用户。
/// 显式按字节数拦截，别让 bcrypt 0.19 静默截断到 72（否则 73/72 字节哈希相同）。
#[sqlx::test]
async fn register_rejects_password_over_72_bytes(pool: PgPool) {
    let svc = service(pool.clone());
    let mut input = valid_input(Some("13800138000"), None);
    input.password = "a".repeat(73); // 73 字节 > 72

    let err = svc.register(input).await.expect_err("超长密码应被拒");
    assert!(
        matches!(err, RegisterError::Password(PasswordError::TooLong)),
        "实际：{err:?}"
    );

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "超长密码不应落半个用户");
}

// ————————————————————— 唯一冲突 → 领域错误映射 —————————————————————
// 注：手机/邮箱冲突映射成同一个 SubjectError::UserAlreadyExists（不分手机/邮箱，你的设计）。

/// 手机号已被占用 → Register(UserAlreadyExists)，且不创建第二个用户。
#[sqlx::test]
async fn register_duplicate_phone_is_rejected(pool: PgPool) {
    let svc = service(pool.clone());
    svc.register(valid_input(Some("13900139000"), None))
        .await
        .expect("首个用户应成功");

    // 同手机号 + 不同邮箱再注册：应映射成 UserAlreadyExists
    let err = svc
        .register(valid_input(Some("13900139000"), Some("other@example.com")))
        .await
        .expect_err("重复手机号应失败");
    assert!(
        matches!(
            err,
            RegisterError::Register(SubjectError::UserAlreadyExists)
        ),
        "实际：{err:?}"
    );

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "重复注册不应新增第二个用户");
}

/// 邮箱已注册，换大小写再注册 → 仍 Register(UserAlreadyExists)（lower(email) 大小写不敏感唯一）。
#[sqlx::test]
async fn register_duplicate_email_is_case_insensitive(pool: PgPool) {
    let svc = service(pool);
    svc.register(valid_input(None, Some("dup@example.com")))
        .await
        .expect("首个用户应成功");

    let err = svc
        .register(valid_input(None, Some("DUP@example.com")))
        .await
        .expect_err("大小写不同的同邮箱应失败");
    assert!(
        matches!(
            err,
            RegisterError::Register(SubjectError::UserAlreadyExists)
        ),
        "实际：{err:?}"
    );
}
