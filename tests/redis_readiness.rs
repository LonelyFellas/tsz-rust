//! `/readyz` 就绪探活：Redis 的健康度必须反映在响应里（见 docs/redis-design.md §3）。
//!
//! 划界：`healthz` 是纯进程存活、不碰任何外部依赖（tests/health.rs 覆盖），这里**只**测
//! `readyz` 对 Redis 的探测——即「就绪」的定义从『DB 通』收紧为『DB 且 Redis 都通』。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sqlx::PgPool;
use tower::ServiceExt;

use tsz_rust::platform;
use tsz_rust::state::AppState;

/// 测试用 Redis 连接串：优先读环境（CI 注入），否则回落到本地默认实例。
/// 与 `#[sqlx::test]` 从 `DATABASE_URL` 取库同理——集成测依赖真 Redis，不 mock。
fn test_redis_url() -> String {
    std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_owned())
}

/// 打 `/readyz`，只取状态码——本组测试只关心「就绪判定」，不关心响应体。
async fn readyz_status(state: AppState) -> StatusCode {
    tsz_rust::router(state)
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[sqlx::test]
async fn readyz_ready_when_db_and_redis_up(pool: PgPool) {
    // DB 由 #[sqlx::test] 保证可用；Redis 指向真实可用实例 → 两者皆通。
    let redis = platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");

    let status = readyz_status(AppState::for_test_with_redis(pool, redis)).await;

    assert_eq!(status, StatusCode::OK, "DB 与 Redis 均通时应 200 ready");
}

#[sqlx::test]
async fn readyz_unavailable_when_redis_down(pool: PgPool) {
    // DB 可用，但 Redis 指向一个无人监听的地址（端口 1）——就绪探活必须因此失败。
    // deadpool 惰性建连：建池即使地址不可达也返回 Ok，连不上要到 readyz 内 ping 时才暴露，
    // 这恰好验证了 readyz **真的探了 Redis**，而不是只探 DB 就返回 ready。
    let dead_redis = platform::connect_redis("redis://127.0.0.1:1")
        .await
        .expect("建池是惰性的，即使地址不可达也应返回 Ok");

    let status = readyz_status(AppState::for_test_with_redis(pool, dead_redis)).await;

    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "Redis 不可达时 readyz 应 503（就绪探活必须覆盖 Redis，而非只探 DB）"
    );
}
