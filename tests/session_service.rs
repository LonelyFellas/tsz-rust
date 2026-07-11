//! `SessionService::issue` 的行为测试（真库，`#[sqlx::test]`）。
//!
//! 验的是 issue 的**落库契约**：存的是哈希不是明文、能按哈希查回、expires_at≈now+ttl、
//! 每次明文都不同。crypto 纯函数（generate/hash）的性质在 `service.rs` 内联单测覆盖。

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use tsz_rust::session::repository::RefreshTokenRepository;
use tsz_rust::session::service::SessionService;

/// FK 依赖：造一个用户，返回 id。phone 用 id 串保证唯一。
async fn seed_user(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO users (id, phone, password_hash, display_name) VALUES ($1, $2, $3, $4)",
        id,
        id.to_string(),
        "hashed-pw",
        "测试用户",
    )
    .execute(pool)
    .await
    .expect("seed user 应成功");
    id
}

/// 镜像 service 里私有的 `hash_token`，用来验证「DB 存的确实是 sha256(明文)」。
/// 若将来改哈希策略（比如加 pepper），这几条会挂——那正是提醒你同步安全契约。
fn expected_hash(plaintext: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(plaintext.as_bytes()))
}

fn service(pool: PgPool, ttl: Duration) -> SessionService {
    SessionService::new(RefreshTokenRepository::new(pool), ttl)
}

/// issue 后：DB 恰好一行、存的是哈希非明文、expires_at≈now+ttl、返回明文给调用方。
#[sqlx::test]
async fn issue_persists_hashed_token_and_returns_plaintext(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let svc = service(pool.clone(), Duration::days(30));

    let before = Utc::now();
    let issued = svc.issue(user_id).await.expect("issue 应成功");
    let after = Utc::now();

    // expires_at ≈ now + 30 天（夹在调用前后各 +30d 之间）
    assert!(
        issued.expires_at >= before + Duration::days(30)
            && issued.expires_at <= after + Duration::days(30),
        "expires_at 应约为 now + ttl"
    );

    // DB 里恰好一行属于该 user
    let rows = sqlx::query!(
        "SELECT token_hash FROM refresh_tokens WHERE user_id = $1",
        user_id
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "issue 应恰好写一行");

    // 核心安全断言：存的是哈希、不是明文，且确实是 sha256(明文)
    let stored = &rows[0].token_hash;
    assert_ne!(stored, &issued.plaintext, "DB 绝不能存明文 refresh token");
    assert_eq!(
        stored,
        &expected_hash(&issued.plaintext),
        "存的应是 base64url(sha256(明文))"
    );
}

/// 模拟 rotate 将来的动作：拿客户端明文 → 算哈希 → find_by_hash 命中。
#[sqlx::test]
async fn issued_token_is_findable_by_its_hash(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool.clone());
    let svc = service(pool, Duration::days(30));

    let issued = svc.issue(user_id).await.expect("issue 应成功");

    let found = repo
        .find_by_hash(&expected_hash(&issued.plaintext))
        .await
        .expect("查询不应报错");
    let row = found.expect("issue 出的 token 应能按其哈希查回");
    assert_eq!(row.user_id, user_id);
    assert!(row.revoked_at.is_none() && row.rotated_at.is_none(), "新发的应是活跃态");
}

/// 两次 issue 明文必须不同（熵够）；DB 里各占一行。
#[sqlx::test]
async fn two_issues_produce_distinct_tokens(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let svc = service(pool.clone(), Duration::days(30));

    let a = svc.issue(user_id).await.unwrap();
    let b = svc.issue(user_id).await.unwrap();
    assert_ne!(a.plaintext, b.plaintext, "两次 issue 明文应不同");

    let count = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM refresh_tokens WHERE user_id = $1",
        user_id
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, Some(2), "应有两行");
}
