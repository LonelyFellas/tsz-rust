//! `OtpService` 编排层集成测试（真 Redis + `OtpSender::Mock`）。
//!
//! 只验 **service 独有的编排**：限流顺序与开关（冷却 → 日限，`0=关闭`）、生成→存→发的贯通、
//! verify 把 store 的 false 统一成 `InvalidCode`。store 原语本身的正确性在 tests/otp_store.rs，
//! 这里不重测。
//!
//! ── 依赖的 `OtpService` 契约（待实现，见本轮骨架）──
//! ```ignore
//! use std::time::Duration;
//! impl OtpService {
//!     pub fn new(store: OtpStore, sender: OtpSender,
//!                ttl: Duration, cooldown: Duration, daily_limit: u64, max_attempts: u8) -> Self;
//!     pub async fn request(&self, target: &str, purpose: Purpose) -> Result<(), OtpServiceError>;
//!     pub async fn verify(&self, target: &str, purpose: Purpose, code: &str) -> Result<(), OtpServiceError>;
//! }
//! pub enum OtpServiceError { RateLimited, InvalidCode, Store(..), Send(..) }
//! ```

use deadpool_redis::{Pool, redis};
use std::time::Duration;
use uuid::Uuid;

use tsz_rust::otp::model::Purpose;
use tsz_rust::otp::sender::OtpSender;
use tsz_rust::otp::service::{OtpService, OtpServiceError};
use tsz_rust::otp::store::OtpStore;
use tsz_rust::platform;

const T: &str = "13800000000";
const TTL: Duration = Duration::from_secs(300);
const MAX_ATTEMPTS: u8 = 5;

fn test_redis_url() -> String {
    std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_owned())
}

/// 建一个带唯一前缀的 service，并返回同一 pool + 前缀（供读回 service 生成并存下的码）。
/// cooldown / daily_limit 由各用例指定以驱动不同的限流分支。
async fn service_with(cooldown: Duration, daily_limit: u64) -> (OtpService, Pool, String) {
    let pool = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let prefix = format!("test:{}:", Uuid::now_v7());
    let store = OtpStore::with_prefix(pool.clone(), prefix.clone());
    let service = OtpService::new(
        store,
        OtpSender::Mock,
        cooldown,
        daily_limit,
        TTL,
        MAX_ATTEMPTS,
    );
    (service, pool, prefix)
}

/// 从 Redis 读回 `request` 刚生成并存下的码（HGET 码键的 `code` 字段）。
/// key 方案与 store 一致：`{prefix}otp:code:{target}:{seg}`。
async fn saved_code(pool: &Pool, prefix: &str, target: &str, purpose: Purpose) -> String {
    let key = format!("{prefix}otp:code:{target}:{}", purpose.as_key_segment());
    let mut conn = pool.get().await.unwrap();
    let code: Option<String> = redis::cmd("HGET")
        .arg(&key)
        .arg("code")
        .query_async(&mut conn)
        .await
        .unwrap();
    code.expect("request 之后应能在 Redis 读到一枚码")
}

/// 造一枚与给定码**保证不同**的错码（末位数字 +1 mod 10）。
/// 确定性命中「码不符」分支，避免硬编码错码撞上真码的 1e-6 假失败。
fn wrong_of(code: &str) -> String {
    let mut bytes = code.as_bytes().to_vec();
    let last = bytes.len() - 1;
    bytes[last] = b'0' + (bytes[last] - b'0' + 1) % 10;
    String::from_utf8(bytes).unwrap()
}

// ======================= 生成 → 存 → 发 贯通 =======================

#[tokio::test]
async fn request_with_mock_sender_saves_fixed_verifiable_code() {
    let (svc, pool, prefix) = service_with(Duration::from_secs(60), 10).await;
    svc.request(T, Purpose::Login)
        .await
        .expect("首次请求应成功");

    // Mock 通道统一落固定码，内部测试环境无需再查日志取码。
    let code = saved_code(&pool, &prefix, T, Purpose::Login).await;
    assert_eq!(code, "000000", "Mock 验证码应统一为 000000");

    // 该码可被 verify 通过 —— 证明 request 存的和 verify 读的是同一枚。
    svc.verify(T, Purpose::Login, &code)
        .await
        .expect("正确码应校验通过");
}

#[tokio::test]
async fn mock_account_deletion_uses_fixed_code_in_its_own_keyspace() {
    let (svc, pool, prefix) = service_with(Duration::from_secs(60), 10).await;
    svc.request(T, Purpose::AccountDeletion)
        .await
        .expect("当前约定允许 Mock 发放固定注销码");
    let code = saved_code(&pool, &prefix, T, Purpose::AccountDeletion).await;
    assert_eq!(code, "000000");
    svc.verify(T, Purpose::AccountDeletion, "000000")
        .await
        .expect("固定注销码应仅在 account_deletion purpose 下可验证");
}

// ============================ verify 映射 ============================

#[tokio::test]
async fn verify_wrong_code_maps_to_invalid() {
    let (svc, pool, prefix) = service_with(Duration::from_secs(60), 10).await;
    svc.request(T, Purpose::Login).await.unwrap();

    // 读回真码，构造一枚保证不同的错码——确定性走「码不符」分支，无 1e-6 假失败。
    let code = saved_code(&pool, &prefix, T, Purpose::Login).await;
    let err = svc
        .verify(T, Purpose::Login, &wrong_of(&code))
        .await
        .unwrap_err();
    assert!(
        matches!(err, OtpServiceError::InvalidCode),
        "错码应映射 InvalidCode，实得 {err:?}"
    );
}

#[tokio::test]
async fn verify_is_single_use() {
    let (svc, pool, prefix) = service_with(Duration::from_secs(60), 10).await;
    svc.request(T, Purpose::Login).await.unwrap();
    let code = saved_code(&pool, &prefix, T, Purpose::Login).await;

    svc.verify(T, Purpose::Login, &code)
        .await
        .expect("首次应通过");
    let err = svc.verify(T, Purpose::Login, &code).await.unwrap_err();
    assert!(
        matches!(err, OtpServiceError::InvalidCode),
        "同码再验应 InvalidCode（单次消费），实得 {err:?}"
    );
}

#[tokio::test]
async fn verify_unknown_target_maps_to_invalid() {
    let (svc, ..) = service_with(Duration::from_secs(60), 10).await;
    // 从未 request 过 → verify 应统一 InvalidCode（不暴露「没发过」，对齐不可区分）。
    let err = svc.verify(T, Purpose::Login, "123456").await.unwrap_err();
    assert!(matches!(err, OtpServiceError::InvalidCode), "实得 {err:?}");
}

// ============================ 冷却 ============================

#[tokio::test]
async fn cooldown_blocks_rapid_second_request() {
    let (svc, ..) = service_with(Duration::from_secs(60), 10).await;
    svc.request(T, Purpose::Login).await.expect("首次应成功");

    let err = svc.request(T, Purpose::Login).await.unwrap_err();
    assert!(
        matches!(err, OtpServiceError::RateLimited),
        "冷却窗内二次请求应 RateLimited，实得 {err:?}"
    );
}

#[tokio::test]
async fn cooldown_zero_disables_cooldown() {
    // 冷却=0 关闭该限制；日限给足。两次快速请求都应成功（不被冷却挡）。
    let (svc, ..) = service_with(Duration::ZERO, 10).await;
    svc.request(T, Purpose::Login).await.expect("首次");
    svc.request(T, Purpose::Login)
        .await
        .expect("冷却关闭后二次也应成功");
}

// ============================ 日限 ============================

#[tokio::test]
async fn daily_limit_enforced_when_cooldown_off() {
    // 冷却=0 以便连发；日限=2：前两次成功，第三次超限。
    let (svc, ..) = service_with(Duration::ZERO, 2).await;
    svc.request(T, Purpose::Login).await.expect("第 1 次");
    svc.request(T, Purpose::Login).await.expect("第 2 次");

    let err = svc.request(T, Purpose::Login).await.unwrap_err();
    assert!(
        matches!(err, OtpServiceError::RateLimited),
        "超日限应 RateLimited，实得 {err:?}"
    );
}

#[tokio::test]
async fn daily_limit_zero_disables_daily() {
    // 冷却=0、日限=0 全关：连发多次都应成功。
    let (svc, ..) = service_with(Duration::ZERO, 0).await;
    for i in 1..=5 {
        svc.request(T, Purpose::Login)
            .await
            .unwrap_or_else(|e| panic!("第 {i} 次应成功，实得 {e:?}"));
    }
}

// ==================== fail-close / purpose 独立 / 锁死恢复 ====================

#[tokio::test]
async fn store_error_fails_closed_not_masked() {
    // Redis 不可达（端口 1 无人监听）→ request/verify 都应以 Store 失败，**绝不**静默成
    // RateLimited/InvalidCode（否则一次外部故障会被当成正常拒绝，掩盖告警——redis-design §9）。
    let dead = platform::connect_redis("redis://127.0.0.1:1")
        .await
        .expect("惰性建池即使地址不可达也应 Ok");
    let store = OtpStore::with_prefix(dead, format!("test:{}:", Uuid::now_v7()));
    let svc = OtpService::new(
        store,
        OtpSender::Mock,
        Duration::from_secs(60),
        10,
        TTL,
        MAX_ATTEMPTS,
    );

    let req_err = svc.request(T, Purpose::Login).await.unwrap_err();
    assert!(
        matches!(req_err, OtpServiceError::Store(_)),
        "Redis 挂时 request 应 fail-close 成 Store，实得 {req_err:?}"
    );
    let vfy_err = svc.verify(T, Purpose::Login, "123456").await.unwrap_err();
    assert!(
        matches!(vfy_err, OtpServiceError::Store(_)),
        "Redis 挂时 verify 应 Store（非 InvalidCode，不掩盖故障），实得 {vfy_err:?}"
    );
}

#[tokio::test]
async fn cooldown_is_independent_per_purpose_at_service_layer() {
    let (svc, ..) = service_with(Duration::from_secs(60), 10).await;
    svc.request(T, Purpose::Login).await.expect("Login 首次");
    // 若 request 把 purpose 写死/丢了，PasswordReset 会撞上 Login 的冷却 → 这里就会 RateLimited。
    svc.request(T, Purpose::PasswordReset)
        .await
        .expect("PasswordReset 应与 Login 的冷却独立");
    // 而同 purpose 再来仍被自己的冷却挡（证明冷却确实生效，不是普遍放行导致的假独立）。
    let err = svc.request(T, Purpose::Login).await.unwrap_err();
    assert!(
        matches!(err, OtpServiceError::RateLimited),
        "Login 二次应被自己的冷却挡，实得 {err:?}"
    );
}

#[tokio::test]
async fn locked_code_recovers_via_new_request() {
    // 防爆破的恢复路径（otp-design §7）：错满 max 锁死后，重新请求应发出一枚能用的新码。
    // 冷却=0 以便锁死后立刻重发。
    let (svc, pool, prefix) = service_with(Duration::ZERO, 10).await;
    svc.request(T, Purpose::Login).await.unwrap();
    let code_a = saved_code(&pool, &prefix, T, Purpose::Login).await;

    // 用与真码保证不同的错码错满 MAX_ATTEMPTS 次，把 code_a 锁死。
    let wrong = wrong_of(&code_a);
    for _ in 0..MAX_ATTEMPTS {
        let err = svc.verify(T, Purpose::Login, &wrong).await.unwrap_err();
        assert!(matches!(err, OtpServiceError::InvalidCode));
    }

    // 重新请求 → 新码；它必须能验通过（证明新码不背旧码锁死的 attempts）。
    svc.request(T, Purpose::Login)
        .await
        .expect("锁死后重发应成功（冷却已关）");
    let code_b = saved_code(&pool, &prefix, T, Purpose::Login).await;
    svc.verify(T, Purpose::Login, &code_b)
        .await
        .expect("重发的新码应能验通过，锁死状态不应残留");
}
