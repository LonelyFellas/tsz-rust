use sqlx::PgPool;
use uuid::Uuid;

const CHECK_VIOLATION: &str = "23514";
const UNIQUE_VIOLATION: &str = "23505";

async fn insert_voice(pool: &PgPool, alias: &str, enabled: bool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"INSERT INTO speech.voices
           (id, alias, provider, provider_voice_id, locale, gender, styles, provider_version, enabled)
           VALUES ($1, $2, 'azure', 'en-US-JennyNeural', 'en-US', 'female', '["chat"]', '2026-08', $3)"#,
    )
    .bind(id)
    .bind(alias)
    .bind(enabled)
    .execute(pool)
    .await
    .expect("voice insert should succeed");
    id
}

fn assert_code<T: std::fmt::Debug>(result: Result<T, sqlx::Error>, code: &str) {
    match result {
        Err(sqlx::Error::Database(error)) => assert_eq!(error.code().as_deref(), Some(code)),
        other => panic!("expected database error {code}, got {other:?}"),
    }
}

#[sqlx::test]
async fn speech_schema_supports_voice_and_preview_cache(pool: PgPool) {
    let voice_id = insert_voice(&pool, "en-us-jenny", true).await;
    sqlx::query(
        r#"INSERT INTO speech.preview_cache
           (request_hash, voice_id, content_hash, object_key, mime_type, size_bytes, expires_at)
           VALUES ($1, $2, $3, 'previews/test.mp3', 'audio/mpeg', 3, now() + interval '1 hour')"#,
    )
    .bind(vec![1_u8; 32])
    .bind(voice_id)
    .bind(vec![2_u8; 32])
    .execute(&pool)
    .await
    .expect("valid preview cache row should insert");

    let row: (String, i64) = sqlx::query_as(
        "SELECT object_key, size_bytes FROM speech.preview_cache WHERE request_hash = $1",
    )
    .bind(vec![1_u8; 32])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, ("previews/test.mp3".to_owned(), 3));
}

#[sqlx::test]
async fn voice_alias_and_cache_constraints_are_enforced(pool: PgPool) {
    insert_voice(&pool, "en-us-jenny", true).await;
    let duplicate = sqlx::query(
        r#"INSERT INTO speech.voices
           (id, alias, provider, provider_voice_id, locale, gender, provider_version)
           VALUES ($1, 'en-us-jenny', 'azure', 'other', 'en-US', 'female', 'v1')"#,
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await;
    assert_code(duplicate, UNIQUE_VIOLATION);

    let invalid = sqlx::query(
        r#"INSERT INTO speech.voices
           (id, alias, provider, provider_voice_id, locale, gender, provider_version)
           VALUES ($1, 'INVALID ALIAS', 'azure', 'voice', 'en-US', 'female', 'v1')"#,
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await;
    assert_code(invalid, CHECK_VIOLATION);
}

#[sqlx::test]
async fn preview_cache_rejects_bad_hash_and_missing_voice(pool: PgPool) {
    let bad_hash = sqlx::query(
        r#"INSERT INTO speech.preview_cache
           (request_hash, voice_id, content_hash, object_key, mime_type, size_bytes, expires_at)
           VALUES ($1, $2, $3, 'previews/a.mp3', 'audio/mpeg', 1, now() + interval '1 hour')"#,
    )
    .bind(vec![1_u8; 31])
    .bind(Uuid::now_v7())
    .bind(vec![2_u8; 32])
    .execute(&pool)
    .await;
    match bad_hash {
        Err(sqlx::Error::Database(error)) => {
            assert!(matches!(error.code().as_deref(), Some("23503" | "23514")))
        }
        other => panic!("invalid row must fail: {other:?}"),
    }
}
