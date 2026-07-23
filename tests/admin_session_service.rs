//! `AdminSessionService`（issue / rotate / logout / peek_admin_id）的行为测试（真库）。
//!
//! web 侧 `SessionService`（tests/session_service.rs）的平移，两处刻意偏离：
//!   - **Q1 严格单登录**：`issue` 前先 `revoke_all_by_admin_id`——后台账号不允许
//!     多处在线，重新登录即挤掉旧会话。
//!   - **Q8 绝对死线**：`rotate` 的新枚**继承**被消费旧枚的 expires_at、不重算
//!     now+ttl——轮换只换凭证不续命，到期 refresh 401 必重走 2FA。
//!     （与 web 的滑动续期刻意相反。）
//!
//! 落库契约照旧：存哈希不存明文、每次明文都不同、rotate 原子换新。
//! crypto 纯函数性质归 src/admin/session.rs 内联单测（若直接复用 web 私有函数的
//! 复制版，性质测试也照抄一份）。
//!
//! 契约（等实现，admin/mod.rs 需 re-export）：
//!   `AdminSessionService::new(AdminRefreshTokenRepository, refresh_ttl: Duration)`
//!   `issue(&Uuid) -> Result<IssuedAdminRefresh, AdminSessionError>`
//!   `rotate(&str) -> Result<RotatedAdminRefresh, AdminSessionError>`
//!   `logout(&str) -> Result<(), AdminSessionError>`
//!   `peek_admin_id(&str) -> Result<Option<Uuid>, AdminSessionError>`
//!   `IssuedAdminRefresh { plaintext, expires_at }`
//!   `RotatedAdminRefresh { admin_id, refresh: IssuedAdminRefresh }`（**刻意无 Debug**，
//!     防新枚明文进日志——所以 expect_err 前一律 `.map(drop)`）
//!   `AdminSessionError::{ InvalidRefreshToken, Repository }`

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use tsz_rust::admin::{
    AdminRefreshTokenRepository, AdminRepository, AdminRole, AdminSessionError,
    AdminSessionService, NewAdmin, NewAdminRefreshToken,
};

/// FK 依赖：造一个管理员，返回 id。phone 用 UUIDv7 串保证唯一。
async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.to_string(),
            display_name: "测试管理员".to_owned(),
            password_hash: "hashed-pw".to_owned(),
            role: AdminRole::Admin,
            must_change_password: true,
        })
        .await
        .expect("seed admin 应成功");
    id
}

/// 镜像 service 私有的 `hash_token`（base64url(sha256(明文))），验证「DB 存的确实是哈希」。
/// 改哈希策略这几条会挂——那正是提醒你同步安全契约。
fn expected_hash(plaintext: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(plaintext.as_bytes()))
}

fn service(pool: PgPool, ttl: Duration) -> AdminSessionService {
    AdminSessionService::new(AdminRefreshTokenRepository::new(pool), ttl)
}

// ————————————————————— issue —————————————————————

/// issue 后：存的是哈希非明文、expires_at ≈ now + ttl、返回明文给调用方。
#[sqlx::test]
async fn issue_persists_hashed_token_and_returns_plaintext(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let svc = service(pool.clone(), Duration::days(7));

    let before = Utc::now();
    let issued = svc.issue(&admin_id).await.expect("issue 应成功");
    let after = Utc::now();

    // 死线 ≈ now + 7 天（夹逼；1s 余量容 DB 微秒截断与时钟粒度）
    assert!(
        issued.expires_at >= before + Duration::days(7) - Duration::seconds(1)
            && issued.expires_at <= after + Duration::days(7),
        "expires_at 应约为 now + ttl"
    );

    let rows = sqlx::query!(
        "SELECT token_hash FROM admin_refresh_tokens WHERE admin_id = $1",
        admin_id
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "issue 应恰好写一行");

    let stored = &rows[0].token_hash;
    assert_ne!(stored, &issued.plaintext, "DB 绝不能存明文 refresh token");
    assert_eq!(
        stored,
        &expected_hash(&issued.plaintext),
        "存的应是 base64url(sha256(明文))"
    );
}

/// **并发 issue（Q1 竞态）**：同一 admin 两个并发登录后，活跃会话数仍必须恒为 1。
/// 钉的是 `revoke_all_and_insert` 的事务 + admins 行锁串行化——若退回「revoke_all 与
/// insert 两条独立语句」，两个 revoke_all 会都跑在两个 insert 之前、各插一枚活跃，
/// 本测试即翻红。多轮放大竞态命中概率。
#[sqlx::test]
async fn concurrent_issue_keeps_single_live_session(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;

    for round in 0..5 {
        let a = service(pool.clone(), Duration::days(7));
        let b = service(pool.clone(), Duration::days(7));
        let (ra, rb) = tokio::join!(a.issue(&admin_id), b.issue(&admin_id));
        ra.unwrap_or_else(|e| panic!("第 {round} 轮并发 issue A 应成功：{e:?}"));
        rb.unwrap_or_else(|e| panic!("第 {round} 轮并发 issue B 应成功：{e:?}"));

        let live = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM admin_refresh_tokens
             WHERE admin_id = $1 AND rotated_at IS NULL AND revoked_at IS NULL AND expires_at > NOW()",
            admin_id
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            live,
            Some(1),
            "第 {round} 轮并发 issue 后活跃会话数必须恒为 1（Q1）"
        );
    }
}

/// **严格单登录（Q1）**：重新 issue 会吊销该 admin 既有的全部会话——旧枚立即失效、
/// 不能再 rotate；任意时刻活跃行数恒为 1；别的 admin 不受波及。
#[sqlx::test]
async fn issue_revokes_all_prior_sessions_of_that_admin(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let bystander = seed_admin(&pool).await;
    let svc = service(pool.clone(), Duration::days(7));

    let first = svc.issue(&admin_id).await.expect("首次登录应成功");
    let other = svc.issue(&bystander).await.expect("路人登录应成功");

    let second = svc.issue(&admin_id).await.expect("重新登录应成功");
    assert_ne!(first.plaintext, second.plaintext, "两次 issue 明文应不同");

    // 旧会话被挤掉：行已吊销、rotate 被拒
    let repo = AdminRefreshTokenRepository::new(pool.clone());
    let old_row = repo
        .find_by_hash(&expected_hash(&first.plaintext))
        .await
        .unwrap()
        .expect("旧枚行应还在（吊销不是删除）");
    assert!(
        old_row.revoked_at.is_some(),
        "重新登录必须吊销旧会话（严格单登录）"
    );
    let err = svc
        .rotate(&first.plaintext)
        .await
        .map(drop)
        .expect_err("被挤掉的旧枚不该还能 rotate");
    assert!(matches!(err, AdminSessionError::InvalidRefreshToken));

    // 新会话可用；该 admin 活跃行恒 1
    svc.rotate(&second.plaintext)
        .await
        .expect("新会话应可正常轮换");
    let live = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM admin_refresh_tokens
         WHERE admin_id = $1 AND rotated_at IS NULL AND revoked_at IS NULL AND expires_at > NOW()",
        admin_id
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live, Some(1), "严格单登录下活跃会话数恒为 1");

    // 别的 admin 毫发无伤
    let other_row = repo
        .find_by_hash(&expected_hash(&other.plaintext))
        .await
        .unwrap()
        .unwrap();
    assert!(
        other_row.revoked_at.is_none(),
        "单登录清场只限本 admin，不得波及他人"
    );
}

// ——————————— rotate（原子轮换 + Q8：继承死线不续命）———————————
// CAS 状态机、事务原子性、继承的 SQL 细节在 admin_session_repository.rs 钉过；
// 这里验 service 的哈希接线、一进一出、死线传递、错误映射。

/// happy：issue 出的明文能被 rotate 消费 → 返回属主 + 新明文；旧行标 rotated、
/// 新行活跃；**新枚死线 = 旧枚死线**（逐值相等，以 DB 回读值为准）。
#[sqlx::test]
async fn rotate_consumes_issued_token_and_keeps_deadline(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool.clone());
    let svc = service(pool, Duration::days(7));

    let issued = svc.issue(&admin_id).await.unwrap();
    // 死线基准取 DB 落库值（微秒精度），不拿内存值比对——TIMESTAMPTZ 截断纳秒会假红
    let deadline = repo
        .find_by_hash(&expected_hash(&issued.plaintext))
        .await
        .unwrap()
        .unwrap()
        .expires_at;

    let got = svc.rotate(&issued.plaintext).await.expect("rotate 应成功");
    assert_eq!(got.admin_id, admin_id, "rotate 应返回属主 admin_id");
    assert_ne!(got.refresh.plaintext, issued.plaintext, "新旧明文应不同");
    assert_eq!(
        got.refresh.expires_at, deadline,
        "新枚死线必须继承旧枚（Q8：轮换不续命）"
    );

    let old_row = repo
        .find_by_hash(&expected_hash(&issued.plaintext))
        .await
        .unwrap()
        .unwrap();
    assert!(old_row.rotated_at.is_some(), "旧行应标上 rotated_at");

    let new_row = repo
        .find_by_hash(&expected_hash(&got.refresh.plaintext))
        .await
        .unwrap()
        .expect("轮换出的新枚应已落库");
    assert_eq!(new_row.admin_id, admin_id);
    assert!(
        new_row.rotated_at.is_none() && new_row.revoked_at.is_none(),
        "新枚应是活跃态"
    );
    assert_eq!(new_row.expires_at, deadline, "落库的新枚死线也必须一致");
}

/// 死线在整条轮换链上恒定：老枚死线设为 now+3d（service ttl 却是 30d），
/// 连轮两次后死线仍是 3d 原值——实现若重算 now+ttl，这里会差出 27 天，立刻现形。
#[sqlx::test]
async fn rotate_chain_never_extends_the_deadline(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool.clone());
    let svc = service(pool, Duration::days(30));

    let plaintext = "known-plaintext-for-deadline-test";
    let inserted = repo
        .insert(NewAdminRefreshToken {
            id: Uuid::now_v7(),
            admin_id,
            token_hash: expected_hash(plaintext),
            expires_at: Utc::now() + Duration::days(3),
        })
        .await
        .unwrap();

    let first = svc.rotate(plaintext).await.expect("第一轮 rotate 应成功");
    assert_eq!(
        first.refresh.expires_at, inserted.expires_at,
        "第一轮死线应 = 登录时的原值（3 天），而非 now + ttl（30 天）"
    );

    let second = svc
        .rotate(&first.refresh.plaintext)
        .await
        .expect("第二轮 rotate 应成功");
    assert_eq!(
        second.refresh.expires_at, inserted.expires_at,
        "轮多少次死线都不动——expires_at 即本次登录的绝对死线"
    );
}

/// 复用即拒：同一明文 rotate 两次，第二次 InvalidRefreshToken。
/// （窗口内不连坐等重放语义在 admin_session_reuse_detection.rs。）
#[sqlx::test]
async fn rotate_same_token_twice_is_rejected(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let svc = service(pool, Duration::days(7));

    let issued = svc.issue(&admin_id).await.unwrap();
    svc.rotate(&issued.plaintext)
        .await
        .expect("首次 rotate 应成功");

    // .map(drop)：RotatedAdminRefresh 含新枚明文，刻意无 Debug（防日志泄露），
    // 而 expect_err 要求 Ok 型实现 Debug。下同。
    let err = svc
        .rotate(&issued.plaintext)
        .await
        .map(drop)
        .expect_err("复用应被拒");
    assert!(
        matches!(err, AdminSessionError::InvalidRefreshToken),
        "复用应是 InvalidRefreshToken，实际 {err:?}"
    );
}

/// 垃圾明文 → InvalidRefreshToken（不 panic、不泄露差别）。
#[sqlx::test]
async fn rotate_garbage_token_is_invalid(pool: PgPool) {
    let svc = service(pool, Duration::days(7));
    let err = svc
        .rotate("definitely-not-a-real-token")
        .await
        .map(drop)
        .expect_err("垃圾串应被拒");
    assert!(matches!(err, AdminSessionError::InvalidRefreshToken));
}

/// 过期明文 → InvalidRefreshToken（绝对死线到点即 401，前端跳登录重走 2FA）。
#[sqlx::test]
async fn rotate_expired_token_is_invalid(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool.clone());
    let svc = service(pool, Duration::days(7));

    let plaintext = "known-plaintext-for-expiry-test";
    repo.insert(NewAdminRefreshToken {
        id: Uuid::now_v7(),
        admin_id,
        token_hash: expected_hash(plaintext),
        expires_at: Utc::now() - Duration::days(1),
    })
    .await
    .unwrap();

    let err = svc
        .rotate(plaintext)
        .await
        .map(drop)
        .expect_err("过期应被拒");
    assert!(matches!(err, AdminSessionError::InvalidRefreshToken));
}

// ————————————————————— logout —————————————————————

/// logout 后该明文不能再 rotate（token 立即失效）。
#[sqlx::test]
async fn logged_out_token_cannot_be_rotated(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let svc = service(pool, Duration::days(7));

    let issued = svc.issue(&admin_id).await.unwrap();
    svc.logout(&issued.plaintext).await.expect("logout 应成功");

    let err = svc
        .rotate(&issued.plaintext)
        .await
        .map(drop)
        .expect_err("已登出的不该能 rotate");
    assert!(matches!(err, AdminSessionError::InvalidRefreshToken));
}

/// logout 幂等 + 不泄露：重复调、以及对不存在的明文，都应 Ok。
#[sqlx::test]
async fn logout_is_idempotent_and_silent_on_unknown(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let svc = service(pool, Duration::days(7));

    let issued = svc.issue(&admin_id).await.unwrap();
    svc.logout(&issued.plaintext)
        .await
        .expect("首次 logout 应 Ok");
    svc.logout(&issued.plaintext)
        .await
        .expect("再次 logout 也应 Ok（幂等）");
    svc.logout("never-existed")
        .await
        .expect("未知明文也应 Ok（不泄露）");
}

// ————————————————————— peek_admin_id —————————————————————
// refresh handler「轮换压轴」次序的地基：先只读定位属主 → 查账号/状态/签名都过了
// → 最后才 rotate。peek 绝不消费。

/// peek 返回属主且**不消费**：peek 之后原枚仍可正常 rotate。
#[sqlx::test]
async fn peek_admin_id_finds_owner_without_consuming(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let svc = service(pool, Duration::days(7));

    let issued = svc.issue(&admin_id).await.unwrap();

    let got = svc
        .peek_admin_id(&issued.plaintext)
        .await
        .expect("peek 不应报错");
    assert_eq!(got, Some(admin_id), "应定位到属主");

    svc.rotate(&issued.plaintext)
        .await
        .expect("peek 只读——之后 rotate 必须照常成功");
}

/// 未知明文 → Ok(None)（未命中不是错误；无效性的统一收敛归 rotate/handler）。
#[sqlx::test]
async fn peek_admin_id_unknown_returns_none(pool: PgPool) {
    let svc = service(pool, Duration::days(7));
    let got = svc
        .peek_admin_id("definitely-not-a-real-token")
        .await
        .expect("peek 不应报错");
    assert!(got.is_none());
}
