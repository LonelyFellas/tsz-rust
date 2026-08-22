//! 试听缓存过期行清理的契约：只删过期 row，未过期 row 必须原样保留。
//!
//! 对应的 OSS 对象不由应用删除，而是由 bucket 生命周期规则按年龄回收，
//! 因此这里只断言数据库行为。

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use tsz_rust::speech::preview::PreviewRepository;
use uuid::Uuid;

async fn insert_voice(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"INSERT INTO speech.voices
           (id, alias, provider, provider_voice_id, locale, gender, styles, provider_version)
           VALUES ($1, 'en-us-jenny', 'azure', 'en-US-JennyNeural', 'en-US', 'female', '[]', '2026-08')"#,
    )
    .bind(id)
    .execute(pool)
    .await
    .expect("voice insert should succeed");
    id
}

/// `created_at` 必须显式给，否则默认 `now()` 会撞上 `expires_at > created_at` 约束。
async fn insert_cache(
    pool: &PgPool,
    voice_id: Uuid,
    tag: u8,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) {
    sqlx::query(
        r#"INSERT INTO speech.preview_cache
           (request_hash, voice_id, content_hash, object_key, mime_type, size_bytes,
            created_at, expires_at)
           VALUES ($1, $2, $1, $3, 'audio/mpeg', 3, $4, $5)"#,
    )
    .bind(vec![tag; 32])
    .bind(voice_id)
    .bind(format!("previews/{tag}.mp3"))
    .bind(created_at)
    .bind(expires_at)
    .execute(pool)
    .await
    .expect("preview cache insert should succeed");
}

#[sqlx::test]
async fn delete_expired_removes_only_expired_rows(pool: PgPool) {
    let voice_id = insert_voice(&pool).await;
    let now = Utc::now();
    insert_cache(
        &pool,
        voice_id,
        1,
        now - Duration::days(2),
        now - Duration::days(1),
    )
    .await;
    insert_cache(
        &pool,
        voice_id,
        2,
        now - Duration::days(3),
        now - Duration::days(2),
    )
    .await;
    insert_cache(
        &pool,
        voice_id,
        3,
        now - Duration::hours(1),
        now + Duration::hours(1),
    )
    .await;

    let deleted = PreviewRepository::new(pool.clone())
        .delete_expired()
        .await
        .expect("cleanup should succeed");

    assert_eq!(deleted, 2, "只应删除两条过期 row");
    let remaining: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT request_hash FROM speech.preview_cache")
            .fetch_all(&pool)
            .await
            .expect("remaining rows should be readable");
    assert_eq!(remaining, vec![vec![3_u8; 32]], "未过期 row 必须保留");
}

#[sqlx::test]
async fn delete_expired_is_idempotent_and_quiet_on_empty_table(pool: PgPool) {
    let repository = PreviewRepository::new(pool.clone());
    assert_eq!(
        repository.delete_expired().await.expect("空表清理不应报错"),
        0
    );

    let voice_id = insert_voice(&pool).await;
    let now = Utc::now();
    insert_cache(
        &pool,
        voice_id,
        1,
        now - Duration::days(2),
        now - Duration::days(1),
    )
    .await;
    assert_eq!(repository.delete_expired().await.expect("首轮应删掉"), 1);
    assert_eq!(
        repository.delete_expired().await.expect("重跑应为空操作"),
        0
    );
}
