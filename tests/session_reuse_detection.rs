//! refresh token **重放检测**的规格测试（真库，`#[sqlx::test]`）。
//!
//! 先于实现写下的验收标准，现已落地于 `SessionService::rotate`（src/session/service.rs），
//! 本组测试即其行为契约。详见 docs/frontend-contract-alignment.md §T3。
//!
//! 契约（RFC 9700 §4.14.2）：
//!   rotate 落空 → find_by_hash 兜底 → 命中且 `rotated_at IS NOT NULL` 且未吊销 = 重放
//!   → `revoke_all_by_user_id(user_id)` 吊销该用户全部会话 + 告警 → 仍返回 `InvalidRefreshToken`。
//!
//! 宽限窗口（grace window）：`rotated_at` 距今 < 20 秒的重放可能只是「响应丢失 /
//! commit 后失败」后的合法重试，服务端无从与攻击区分——取舍是窗口内**只回 401、
//! 不连坐**（那台设备重登即可，别的设备无辜）。宽限只软化「连坐」这一件事：
//! **绝不发新枚、绝不复活已吊销/已过期的 token**。窗口外的重放场景在测试里用
//! `backdate_rotated_at` 把轮换时间回拨出窗口来构造。
//!
//! 这组测试钉五件事，缺一不可：
//!   1. （窗口外的）重放**确实**触发全量吊销（功能本身）
//!   2. 只炸受害者一个人（爆炸半径）
//!   3. 非重放的失败（垃圾串 / 过期 / 已登出）**绝不**误伤（假阳性 —— 比漏检更该防，
//!      因为它会把正常用户全端踢下线）
//!   4. 对外错误始终是 `InvalidRefreshToken`，不告诉攻击者"你被识破了"
//!   5. 宽限窗口内的重放：不连坐、也**不放行**（401 但不 revoke_all，更不发新枚）
//!
//! 注：rotate 的 happy path、CAS 状态机、错误映射在 `session_service.rs` /
//! `session_repository.rs` 已覆盖，这里不重复。

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use tsz_rust::session::model::NewRefreshToken;
use tsz_rust::session::repository::RefreshTokenRepository;
use tsz_rust::session::service::{SessionError, SessionService};

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

/// 镜像 service 私有的 `hash_token`，用来直接查行状态。
fn expected_hash(plaintext: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(plaintext.as_bytes()))
}

fn service(pool: PgPool, ttl: Duration) -> SessionService {
    SessionService::new(RefreshTokenRepository::new(pool), ttl)
}

/// 这枚明文对应的行是否已被吊销（`revoked_at` 非空）。
async fn is_revoked(pool: &PgPool, plaintext: &str) -> bool {
    RefreshTokenRepository::new(pool.clone())
        .find_by_hash(&expected_hash(plaintext))
        .await
        .expect("查询不应报错")
        .expect("该行应存在")
        .revoked_at
        .is_some()
}

/// 宽限窗口秒数的测试侧镜像（生产值在 `SessionService::rotate`）。改生产值时同步改这里。
const GRACE_SECS: i64 = 20;

/// 把这枚明文对应行的 `rotated_at` 回拨 `secs` 秒——把「刚轮换」变成「窗口外的旧案」。
async fn backdate_rotated_at(pool: &PgPool, plaintext: &str, secs: i64) {
    let n = sqlx::query!(
        "UPDATE refresh_tokens SET rotated_at = rotated_at - make_interval(secs => $1) WHERE token_hash = $2",
        secs as f64,
        expected_hash(plaintext)
    )
    .execute(pool)
    .await
    .expect("回拨 rotated_at 应成功")
    .rows_affected();
    assert_eq!(
        n, 1,
        "应恰好回拨一行（行不存在或 rotated_at 为 NULL 都是用法错误）"
    );
}

/// 该用户当前活跃（未轮换、未吊销、未过期）的 token 行数。
async fn count_live(pool: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar!(
        "SELECT COUNT(*) FROM refresh_tokens
         WHERE user_id = $1 AND rotated_at IS NULL AND revoked_at IS NULL AND expires_at > NOW()",
        user_id
    )
    .fetch_one(pool)
    .await
    .expect("计数不应报错")
    .unwrap_or(0)
}

// ————————————————————— 1. 重放触发全量吊销 —————————————————————

/// 核心场景：攻击者偷到 A、用户先用 A 换成了 A'，攻击者再拿 A 来换。
///
/// 时间线（RFC 9700 §4.14.2 的经典链）：
///   issue A（登录）→ rotate(A) 原子轮换出 A'
///   另一台设备还有 B 在用
///   → 攻击者 replay A → 检测到重放 → **A' 和 B 全部作废，用户被强制全端重登**
///
/// 若检测缺失（replay 只回 401、A' 照常能用），攻击者手里若是 A'，他能一直续下去。
#[sqlx::test]
async fn replaying_rotated_token_revokes_the_whole_chain(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let svc = service(pool.clone(), Duration::days(30));

    // 设备一：登录拿 A，正常刷一次 → A 被消费，原子轮换直接给出 A'
    let a = svc.issue(user_id).await.unwrap();
    let a_prime = svc
        .rotate(&a.plaintext)
        .await
        .expect("首次 rotate 应成功")
        .refresh;

    // 设备二：另一个活跃会话
    let b = svc.issue(user_id).await.unwrap();

    // 把 A 的轮换时间回拨出宽限窗口——窗口内的重放按丢包重试宽待，不算铁证（见第 5 节）
    backdate_rotated_at(&pool, &a.plaintext, GRACE_SECS + 5).await;

    // 攻击者拿着偷来的旧枚 A 再来一次 —— 这是泄露的铁证
    // .map(drop)：RotatedRefresh 含新枚明文，刻意无 Debug（防日志泄露），
    // 而 expect_err 要求 Ok 型实现 Debug。下同。
    let err = svc
        .rotate(&a.plaintext)
        .await
        .map(drop)
        .expect_err("重放应被拒");
    assert!(
        matches!(err, SessionError::InvalidRefreshToken),
        "重放对外仍应是 InvalidRefreshToken（别告诉攻击者被识破），实际 {err:?}"
    );

    // 检测到重放 → 该用户所有会话作废
    assert!(
        is_revoked(&pool, &a_prime.plaintext).await,
        "重放被检测后，同链上的 A' 必须作废（否则攻击者手握 A' 可无限续期）"
    );
    assert!(
        is_revoked(&pool, &b.plaintext).await,
        "重放被检测后，该用户其它设备的会话 B 也必须作废"
    );

    // 作废要是真的：A' 不能再换
    let err = svc
        .rotate(&a_prime.plaintext)
        .await
        .map(drop)
        .expect_err("已被全量吊销的 A' 不该还能 rotate");
    assert!(matches!(err, SessionError::InvalidRefreshToken));
}

// ————————————————————— 2. 爆炸半径 —————————————————————

/// 只炸受害者：user1 的重放不能波及 user2 的会话。
///
/// `revoke_all_by_user_id` 若把 user_id 传错（比如漏了 WHERE），这条会挂。
#[sqlx::test]
async fn reuse_detection_is_scoped_to_the_victim(pool: PgPool) {
    let victim = seed_user(&pool).await;
    let bystander = seed_user(&pool).await;
    let svc = service(pool.clone(), Duration::days(30));

    // 受害者：制造一枚被消费过的 token（回拨出宽限窗口，构成铁证）
    let stolen = svc.issue(victim).await.unwrap();
    svc.rotate(&stolen.plaintext).await.unwrap();
    backdate_rotated_at(&pool, &stolen.plaintext, GRACE_SECS + 5).await;

    // 路人甲：一个无关的活跃会话
    let innocent = svc.issue(bystander).await.unwrap();

    // 受害者这边触发重放检测
    svc.rotate(&stolen.plaintext)
        .await
        .map(drop)
        .expect_err("重放应被拒");

    // 路人甲毫发无伤，且还能正常刷
    assert!(
        !is_revoked(&pool, &innocent.plaintext).await,
        "别的用户的会话不该被波及"
    );
    let owner = svc
        .rotate(&innocent.plaintext)
        .await
        .expect("路人甲的 token 应仍可正常 rotate")
        .user_id;
    assert_eq!(owner, bystander);
}

// ————————————————————— 3. 假阳性：非重放的失败绝不误伤 —————————————————————
//
// 这三条比"检测到重放"更要紧：误判会把正常用户全端踢下线。
// 判据只有一条 —— `rotated_at IS NOT NULL`。其余失败一律只回 401，不许动任何行。

/// 垃圾串（压根查不到行）不能触发全量吊销。
#[sqlx::test]
async fn unknown_token_does_not_trigger_mass_revocation(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let svc = service(pool.clone(), Duration::days(30));

    let live = svc.issue(user_id).await.unwrap();

    svc.rotate("definitely-not-a-real-token")
        .await
        .map(drop)
        .expect_err("垃圾串应被拒");

    assert!(
        !is_revoked(&pool, &live.plaintext).await,
        "查不到的 token 不是重放证据，不该吊销任何东西"
    );
    svc.rotate(&live.plaintext)
        .await
        .expect("活跃 token 应不受影响");
}

/// 过期但**没被用过**的 token（`rotated_at` 为空）不是重放 —— 只是过期，不许误伤。
#[sqlx::test]
async fn expired_token_is_not_treated_as_reuse(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool.clone());
    let svc = service(pool.clone(), Duration::days(30));

    // 直插一枚「明文已知、已过期、从未被消费」的行
    let expired = "known-plaintext-expired-never-used";
    repo.insert(NewRefreshToken {
        id: Uuid::now_v7(),
        user_id,
        token_hash: expected_hash(expired),
        expires_at: Utc::now() - Duration::days(1),
    })
    .await
    .unwrap();

    // 同一用户另有活跃会话
    let live = svc.issue(user_id).await.unwrap();

    svc.rotate(expired).await.map(drop).expect_err("过期应被拒");

    assert!(
        !is_revoked(&pool, &live.plaintext).await,
        "过期 ≠ 重放（rotated_at 为空）；把过期当泄露会天天误杀正常用户"
    );
    svc.rotate(&live.plaintext)
        .await
        .expect("活跃 token 应不受影响");
}

/// 已登出（`revoked_at` 非空、`rotated_at` 为空）的 token 被重放，不算泄露证据。
///
/// 这条钉的是 §T3 的设计取舍：**只有 `rotated_at` 非空才算重放**。
/// 理由：logout 后用户自己的老 tab 拿着废 token 再刷一次是很常见的，
/// 那不是攻击，不该把他其它设备一起踢下线。
#[sqlx::test]
async fn revoked_token_replay_is_not_treated_as_reuse(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let svc = service(pool.clone(), Duration::days(30));

    let logged_out = svc.issue(user_id).await.unwrap();
    let other_device = svc.issue(user_id).await.unwrap();

    svc.logout(&logged_out.plaintext)
        .await
        .expect("logout 应成功");

    svc.rotate(&logged_out.plaintext)
        .await
        .map(drop)
        .expect_err("已吊销的 token 应被拒");

    assert!(
        !is_revoked(&pool, &other_device.plaintext).await,
        "登出后的废 token 被重放 ≠ 泄露，不该殃及其它设备"
    );
    svc.rotate(&other_device.plaintext)
        .await
        .expect("其它设备的会话应不受影响");
}

// ————————————————————— 4. 不泄露检测结果 —————————————————————

/// 重放 / 垃圾串 / 过期 三者对外必须是**同一个**错误，攻击者无法据此探测。
///
/// 这条现在是绿的（因为眼下全都无脑回 InvalidRefreshToken）——它的价值在于：
/// 实现重放检测时，别顺手加个 `TokenReused` 变体或把告警写进错误体漏出去。
#[sqlx::test]
async fn all_rejection_paths_return_the_same_opaque_error(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool.clone());
    let svc = service(pool.clone(), Duration::days(30));

    // (a) 重放（回拨出宽限窗口）
    let reused = svc.issue(user_id).await.unwrap();
    svc.rotate(&reused.plaintext).await.unwrap();
    backdate_rotated_at(&pool, &reused.plaintext, GRACE_SECS + 5).await;
    let reuse_err = svc
        .rotate(&reused.plaintext)
        .await
        .map(drop)
        .expect_err("重放应被拒");

    // (b) 垃圾串
    let unknown_err = svc
        .rotate("garbage")
        .await
        .map(drop)
        .expect_err("垃圾串应被拒");

    // (c) 过期
    let expired = "known-plaintext-for-opacity-test";
    repo.insert(NewRefreshToken {
        id: Uuid::now_v7(),
        user_id: seed_user(&pool).await,
        token_hash: expected_hash(expired),
        expires_at: Utc::now() - Duration::days(1),
    })
    .await
    .unwrap();
    let expired_err = svc.rotate(expired).await.map(drop).expect_err("过期应被拒");

    for (label, err) in [
        ("重放", &reuse_err),
        ("垃圾串", &unknown_err),
        ("过期", &expired_err),
    ] {
        assert!(
            matches!(err, SessionError::InvalidRefreshToken),
            "{label} 应是 InvalidRefreshToken，实际 {err:?}"
        );
    }

    // 错误信息本身也不能有差别（Display 会进日志/响应体）
    assert_eq!(
        reuse_err.to_string(),
        unknown_err.to_string(),
        "重放与垃圾串的错误文案必须一致，否则攻击者能探测出'这枚曾经有效'"
    );
    assert_eq!(
        reuse_err.to_string(),
        expired_err.to_string(),
        "重放与过期的错误文案必须一致"
    );
}

// ————————————————————— 5. 宽限窗口：窗口内不连坐、也不放行 —————————————————————
//
// 「响应丢失 / commit 后失败」的合法重试与攻击者的即时重放，服务端不可区分。
// 取舍：窗口内一律只回 401（那台设备重新登录），不 revoke_all（别的设备无辜）。
// 但绝不能因为"可能是自己人"就发新枚——那等于给窗口内的攻击者发通行证。

/// 窗口内重放 → 401，但不吊销任何会话：A' 和 B 都活着、都能继续用。
/// （丢包重试的用户代价 = 一台设备重登，而不是全端下线。）
#[sqlx::test]
async fn in_grace_replay_is_401_without_mass_revocation(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let svc = service(pool.clone(), Duration::days(30));

    let a = svc.issue(user_id).await.unwrap();
    let b = svc.issue(user_id).await.unwrap();
    let a_prime = svc
        .rotate(&a.plaintext)
        .await
        .expect("首次 rotate 应成功")
        .refresh;

    // 紧接着重放（rotated_at 距今 << 宽限窗口）
    let err = svc
        .rotate(&a.plaintext)
        .await
        .map(drop)
        .expect_err("窗口内重放也必须 401——宽限不等于放行，否则窗口内的攻击者直接拿到通行证");
    assert!(matches!(err, SessionError::InvalidRefreshToken));

    // 宽限的意义：谁都不连坐
    assert!(
        !is_revoked(&pool, &a_prime.plaintext).await,
        "窗口内重放不该吊销 A'——这正是宽限要防的误伤"
    );
    assert!(
        !is_revoked(&pool, &b.plaintext).await,
        "窗口内重放不该连坐设备二的 B"
    );

    // 活着要是真的：链条能续、B 能刷
    svc.rotate(&a_prime.plaintext)
        .await
        .expect("A' 应仍可正常轮换");
    svc.rotate(&b.plaintext).await.expect("B 应仍可正常轮换");
}

/// 窗口内重放不得凭空造会话：重放前后，该用户的活跃 token 行数必须不变。
/// （钉住「宽限 = 发新枚」这类实现：攻击者每隔一小会儿重放一次即可无限铸币。）
#[sqlx::test]
async fn in_grace_replay_mints_nothing(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let svc = service(pool.clone(), Duration::days(30));

    let a = svc.issue(user_id).await.unwrap();
    svc.rotate(&a.plaintext).await.expect("首次 rotate 应成功");

    let live_before = count_live(&pool, user_id).await;
    let _ = svc.rotate(&a.plaintext).await; // 窗口内重放——无论实现回什么
    assert_eq!(
        count_live(&pool, user_id).await,
        live_before,
        "窗口内重放前后活跃行数必须不变——宽限绝不发新枚"
    );
}

/// 已登出的 token 没有宽限：logout 后立刻重放 → 401，且不得复活出任何新会话。
/// （宽限判据只看 `rotated_at`；`revoked_at` 非空 = 用户明确说过"这枚作废"。）
#[sqlx::test]
async fn grace_does_not_resurrect_revoked_tokens(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let svc = service(pool.clone(), Duration::days(30));

    let t = svc.issue(user_id).await.unwrap();
    svc.logout(&t.plaintext).await.expect("logout 应成功");

    let live_before = count_live(&pool, user_id).await;
    let err = svc
        .rotate(&t.plaintext)
        .await
        .map(drop)
        .expect_err("已登出的 token 无宽限可言——否则 logout 形同虚设");
    assert!(matches!(err, SessionError::InvalidRefreshToken));
    assert_eq!(
        count_live(&pool, user_id).await,
        live_before,
        "登出态不得被宽限复活出新会话"
    );
}

/// 过期 token 同理没有宽限：过期 ≠ 刚轮换，不得借宽限还魂。
#[sqlx::test]
async fn grace_does_not_resurrect_expired_tokens(pool: PgPool) {
    let user_id = seed_user(&pool).await;
    let repo = RefreshTokenRepository::new(pool.clone());
    let svc = service(pool.clone(), Duration::days(30));

    let expired = "known-plaintext-expired-for-grace-test";
    repo.insert(NewRefreshToken {
        id: Uuid::now_v7(),
        user_id,
        token_hash: expected_hash(expired),
        expires_at: Utc::now() - Duration::days(1),
    })
    .await
    .unwrap();

    let err = svc
        .rotate(expired)
        .await
        .map(drop)
        .expect_err("过期 token 无宽限可言");
    assert!(matches!(err, SessionError::InvalidRefreshToken));
    assert_eq!(
        count_live(&pool, user_id).await,
        0,
        "过期 token 不得被宽限复活出新会话"
    );
}
