//! admin refresh token **重放检测**的规格测试（真库，`#[sqlx::test]`）。
//!
//! web 侧 tests/session_reuse_detection.rs 的平移（契约同 RFC 9700 §4.14.2 +
//! 20s 宽限窗口不铸币不连坐），叠加 admin 的严格单登录（Q1）后场景有两处变形：
//!   - 「多设备连坐」不存在——单登录下受害者最多只有一条活链，重放连坐 =
//!     炸掉链上现存的那枚新 token。
//!   - 「被挤掉的旧会话重放」成为高频合法场景：重新登录后老 tab 拿着已吊销的
//!     旧枚再刷，**不是**泄露证据（revoked_at 非空、rotated_at 为空），绝不许
//!     把刚登录的新会话连坐掉——否则单登录 + 误伤 = 谁也登不进来。
//!
//! 五件事缺一不可（同 web）：
//!   1. 窗口外重放确实触发 revoke_all（功能本身）
//!   2. 只炸受害者一个 admin（爆炸半径）
//!   3. 非重放失败（垃圾串/过期/已吊销）绝不误伤（假阳性比漏检更该防）
//!   4. 对外错误始终是同一个 InvalidRefreshToken（不告诉攻击者被识破）
//!   5. 宽限窗口内：不连坐、不铸币、不复活已吊销/已过期

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use tsz_rust::admin::{
    AdminRefreshTokenRepository, AdminRepository, AdminRole, AdminSessionError,
    AdminSessionService, NewAdmin, NewAdminRefreshToken,
};

/// 宽限窗口秒数的测试侧镜像（生产值在 `AdminSessionService::rotate`，沿 web 20s）。
/// 改生产值时同步改这里。
const GRACE_SECS: i64 = 20;

/// FK 依赖：造一个管理员，返回 id。
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

/// 镜像 service 私有的 `hash_token`，用来直接查行状态。
fn expected_hash(plaintext: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(plaintext.as_bytes()))
}

fn service(pool: PgPool, ttl: Duration) -> AdminSessionService {
    AdminSessionService::new(AdminRefreshTokenRepository::new(pool), ttl)
}

/// 这枚明文对应的行是否已被吊销。
async fn is_revoked(pool: &PgPool, plaintext: &str) -> bool {
    AdminRefreshTokenRepository::new(pool.clone())
        .find_by_hash(&expected_hash(plaintext))
        .await
        .expect("查询不应报错")
        .expect("该行应存在")
        .revoked_at
        .is_some()
}

/// 把这枚明文对应行的 `rotated_at` 回拨 `secs` 秒——把「刚轮换」变成「窗口外的旧案」。
async fn backdate_rotated_at(pool: &PgPool, plaintext: &str, secs: i64) {
    let n = sqlx::query!(
        "UPDATE admin_refresh_tokens SET rotated_at = rotated_at - make_interval(secs => $1) WHERE token_hash = $2",
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

/// 该 admin 当前活跃（未轮换、未吊销、未过期）的 token 行数。
async fn count_live(pool: &PgPool, admin_id: Uuid) -> i64 {
    sqlx::query_scalar!(
        "SELECT COUNT(*) FROM admin_refresh_tokens
         WHERE admin_id = $1 AND rotated_at IS NULL AND revoked_at IS NULL AND expires_at > NOW()",
        admin_id
    )
    .fetch_one(pool)
    .await
    .expect("计数不应报错")
    .unwrap_or(0)
}

// ————————————————————— 1. 重放触发全量吊销 —————————————————————

/// 核心场景：攻击者偷到 A，管理员先用 A 换成了 A'，攻击者（窗口外）再拿 A 来换
/// → A' 必须作废（单登录下这就是受害者的全部会话）。
#[sqlx::test]
async fn replaying_rotated_token_revokes_the_live_session(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let svc = service(pool.clone(), Duration::days(7));

    let a = svc.issue(&admin_id).await.unwrap();
    let a_prime = svc
        .rotate(&a.plaintext)
        .await
        .expect("首次 rotate 应成功")
        .refresh;

    // 把 A 的轮换时间回拨出宽限窗口——窗口内的重放按丢包重试宽待（见第 5 节）
    backdate_rotated_at(&pool, &a.plaintext, GRACE_SECS + 5).await;

    // .map(drop)：RotatedAdminRefresh 含新枚明文，刻意无 Debug。下同。
    let err = svc
        .rotate(&a.plaintext)
        .await
        .map(drop)
        .expect_err("重放应被拒");
    assert!(
        matches!(err, AdminSessionError::InvalidRefreshToken),
        "重放对外仍应是 InvalidRefreshToken（别告诉攻击者被识破），实际 {err:?}"
    );

    assert!(
        is_revoked(&pool, &a_prime.plaintext).await,
        "重放被检测后，链上的 A' 必须作废（否则攻击者手握 A' 可续到死线）"
    );
    let err = svc
        .rotate(&a_prime.plaintext)
        .await
        .map(drop)
        .expect_err("已被连坐吊销的 A' 不该还能 rotate");
    assert!(matches!(err, AdminSessionError::InvalidRefreshToken));
}

// ————————————————————— 2. 爆炸半径 —————————————————————

/// 只炸受害者：admin1 的重放不能波及 admin2 的会话。
#[sqlx::test]
async fn reuse_detection_is_scoped_to_the_victim(pool: PgPool) {
    let victim = seed_admin(&pool).await;
    let bystander = seed_admin(&pool).await;
    let svc = service(pool.clone(), Duration::days(7));

    let stolen = svc.issue(&victim).await.unwrap();
    svc.rotate(&stolen.plaintext).await.unwrap();
    backdate_rotated_at(&pool, &stolen.plaintext, GRACE_SECS + 5).await;

    let innocent = svc.issue(&bystander).await.unwrap();

    svc.rotate(&stolen.plaintext)
        .await
        .map(drop)
        .expect_err("重放应被拒");

    assert!(
        !is_revoked(&pool, &innocent.plaintext).await,
        "别的 admin 的会话不该被波及"
    );
    let owner = svc
        .rotate(&innocent.plaintext)
        .await
        .expect("路人的 token 应仍可正常 rotate")
        .admin_id;
    assert_eq!(owner, bystander);
}

// ——————————— 3. 假阳性：非重放的失败绝不误伤 ———————————
// 判据只有一条：`rotated_at IS NOT NULL` 且未吊销。其余失败一律只回 401，不动任何行。

/// 垃圾串（压根查不到行）不能触发全量吊销。
#[sqlx::test]
async fn unknown_token_does_not_trigger_mass_revocation(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let svc = service(pool.clone(), Duration::days(7));

    let live = svc.issue(&admin_id).await.unwrap();

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
        .expect("活跃会话应不受影响");
}

/// 过期但**没被用过**的 token（rotated_at 为空）不是重放——只是死线到了，不许误伤。
#[sqlx::test]
async fn expired_token_is_not_treated_as_reuse(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool.clone());
    let svc = service(pool.clone(), Duration::days(7));

    // 先建活跃会话，再直插过期行——顺序反过来会被严格单登录的 issue 清场吊销掉
    let live = svc.issue(&admin_id).await.unwrap();
    let expired = "known-plaintext-expired-never-used";
    repo.insert(NewAdminRefreshToken {
        id: Uuid::now_v7(),
        admin_id,
        token_hash: expected_hash(expired),
        expires_at: Utc::now() - Duration::days(1),
    })
    .await
    .unwrap();

    svc.rotate(expired).await.map(drop).expect_err("过期应被拒");

    assert!(
        !is_revoked(&pool, &live.plaintext).await,
        "过期 ≠ 重放（rotated_at 为空）；7 天死线到点是常态，误判会天天连坐"
    );
    svc.rotate(&live.plaintext)
        .await
        .expect("活跃会话应不受影响");
}

/// **单登录高频场景**：重新登录挤掉旧会话（revoked_at 非空、rotated_at 为空）后，
/// 老 tab 拿旧枚再刷——那是自己人不是攻击，绝不许连坐刚登录的新会话。
/// （误判的后果在单登录下被放大：挤掉→重放→连坐新会话 = 永远登不进来。）
#[sqlx::test]
async fn displaced_session_replay_is_not_treated_as_reuse(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let svc = service(pool.clone(), Duration::days(7));

    let old_login = svc.issue(&admin_id).await.unwrap();
    let new_login = svc.issue(&admin_id).await.unwrap(); // 单登录：old_login 在此被吊销

    svc.rotate(&old_login.plaintext)
        .await
        .map(drop)
        .expect_err("被挤掉的旧枚应被拒");

    assert!(
        !is_revoked(&pool, &new_login.plaintext).await,
        "被挤掉的旧枚重放 ≠ 泄露，不得连坐刚登录的新会话"
    );
    svc.rotate(&new_login.plaintext)
        .await
        .expect("新会话应不受影响");
}

// ————————————————————— 4. 不泄露检测结果 —————————————————————

/// 重放 / 垃圾串 / 过期 对外必须是**同一个**错误（变体与文案都一致），攻击者无法探测。
#[sqlx::test]
async fn all_rejection_paths_return_the_same_opaque_error(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool.clone());
    let svc = service(pool.clone(), Duration::days(7));

    // (a) 窗口外重放
    let reused = svc.issue(&admin_id).await.unwrap();
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
    repo.insert(NewAdminRefreshToken {
        id: Uuid::now_v7(),
        admin_id: seed_admin(&pool).await,
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
            matches!(err, AdminSessionError::InvalidRefreshToken),
            "{label} 应是 InvalidRefreshToken，实际 {err:?}"
        );
    }

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

// ————————————— 5. 宽限窗口：窗口内不连坐、也不放行 —————————————

/// 窗口内重放 → 401，但不吊销：A' 活着、能继续用。
/// （丢包重试的代价 = 这一次 401 后前端重登，而不是把刚换出的链也炸掉。）
#[sqlx::test]
async fn in_grace_replay_is_401_without_revocation(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let svc = service(pool.clone(), Duration::days(7));

    let a = svc.issue(&admin_id).await.unwrap();
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
        .expect_err("窗口内重放也必须 401——宽限不等于放行");
    assert!(matches!(err, AdminSessionError::InvalidRefreshToken));

    assert!(
        !is_revoked(&pool, &a_prime.plaintext).await,
        "窗口内重放不该连坐 A'——这正是宽限要防的误伤"
    );
    svc.rotate(&a_prime.plaintext)
        .await
        .expect("A' 应仍可正常轮换");
}

/// 窗口内重放不得凭空造会话：重放前后活跃行数必须不变（宽限绝不铸币）。
#[sqlx::test]
async fn in_grace_replay_mints_nothing(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let svc = service(pool.clone(), Duration::days(7));

    let a = svc.issue(&admin_id).await.unwrap();
    svc.rotate(&a.plaintext).await.expect("首次 rotate 应成功");

    let live_before = count_live(&pool, admin_id).await;
    let _ = svc.rotate(&a.plaintext).await; // 窗口内重放——无论实现回什么
    assert_eq!(
        count_live(&pool, admin_id).await,
        live_before,
        "窗口内重放前后活跃行数必须不变——宽限绝不发新枚"
    );
}

/// 已登出的 token 没有宽限：logout 后立刻重放 → 401，不得复活出任何新会话。
#[sqlx::test]
async fn grace_does_not_resurrect_logged_out_tokens(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let svc = service(pool.clone(), Duration::days(7));

    let t = svc.issue(&admin_id).await.unwrap();
    svc.logout(&t.plaintext).await.expect("logout 应成功");

    let live_before = count_live(&pool, admin_id).await;
    let err = svc
        .rotate(&t.plaintext)
        .await
        .map(drop)
        .expect_err("已登出的 token 无宽限可言——否则 logout 形同虚设");
    assert!(matches!(err, AdminSessionError::InvalidRefreshToken));
    assert_eq!(
        count_live(&pool, admin_id).await,
        live_before,
        "登出态不得被宽限复活出新会话"
    );
}

/// 过期 token 同理没有宽限：过期 ≠ 刚轮换，不得借宽限还魂续过死线。
#[sqlx::test]
async fn grace_does_not_resurrect_expired_tokens(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = AdminRefreshTokenRepository::new(pool.clone());
    let svc = service(pool.clone(), Duration::days(7));

    let expired = "known-plaintext-expired-for-grace-test";
    repo.insert(NewAdminRefreshToken {
        id: Uuid::now_v7(),
        admin_id,
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
    assert!(matches!(err, AdminSessionError::InvalidRefreshToken));
    assert_eq!(
        count_live(&pool, admin_id).await,
        0,
        "过期 token 不得被宽限复活出新会话"
    );
}
