use sqlx::PgPool;
use tsz_rust::admin::{AdminRepository, AdminRole, NewAdmin};
use uuid::Uuid;

const CHECK_VIOLATION: &str = "23514";
const UNIQUE_VIOLATION: &str = "23505";
const FOREIGN_KEY_VIOLATION: &str = "23503";

async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("audio-{}", id.simple()),
            display_name: "Audio Admin".to_owned(),
            password_hash: "hash".to_owned(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    id
}

#[allow(clippy::too_many_arguments)]
async fn insert(
    pool: &PgPool,
    admin_id: Uuid,
    object_key: &str,
    content_type: &str,
    size_bytes: i64,
    locale: &str,
    gender: &str,
    original_name: &str,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO lexicon.audio_assets
           (id, object_key, content_type, size_bytes, locale, gender, original_name, created_by_admin_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
    )
    .bind(Uuid::now_v7())
    .bind(object_key)
    .bind(content_type)
    .bind(size_bytes)
    .bind(locale)
    .bind(gender)
    .bind(original_name)
    .bind(admin_id)
    .execute(pool)
    .await
}

fn assert_code<T: std::fmt::Debug>(result: Result<T, sqlx::Error>, code: &str) {
    match result {
        Err(sqlx::Error::Database(error)) => assert_eq!(error.code().as_deref(), Some(code)),
        other => panic!("expected database error {code}, got {other:?}"),
    }
}

#[sqlx::test]
async fn audio_assets_accept_valid_rows_with_null_duration(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    insert(
        &pool,
        admin_id,
        "assets/first.mp3",
        "audio/mpeg",
        1024,
        "en-GB",
        "female",
        "slow.mp3",
    )
    .await
    .expect("valid audio asset should insert");

    let row: (i64, Option<i32>) = sqlx::query_as(
        "SELECT size_bytes, duration_ms FROM lexicon.audio_assets WHERE object_key = $1",
    )
    .bind("assets/first.mp3")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (1024, None));
}

#[sqlx::test]
async fn audio_asset_object_key_is_unique(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    insert(
        &pool,
        admin_id,
        "assets/dup.mp3",
        "audio/mpeg",
        1,
        "en-US",
        "male",
        "a.mp3",
    )
    .await
    .unwrap();
    assert_code(
        insert(
            &pool,
            admin_id,
            "assets/dup.mp3",
            "audio/wav",
            1,
            "en-US",
            "male",
            "b.wav",
        )
        .await,
        UNIQUE_VIOLATION,
    );
}

#[sqlx::test]
async fn audio_asset_domain_constraints_are_enforced(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    for (label, content_type, size, locale, gender, name) in [
        ("content type", "audio/flac", 1, "en-GB", "female", "a.flac"),
        ("size", "audio/mpeg", 0, "en-GB", "female", "a.mp3"),
        ("locale", "audio/mpeg", 1, "en-AU", "female", "a.mp3"),
        ("gender", "audio/mpeg", 1, "en-GB", "neutral", "a.mp3"),
        ("empty name", "audio/mpeg", 1, "en-GB", "female", ""),
    ] {
        let result = insert(
            &pool,
            admin_id,
            &format!("assets/{}.mp3", Uuid::now_v7()),
            content_type,
            size,
            locale,
            gender,
            name,
        )
        .await;
        assert_code(result, CHECK_VIOLATION);
        println!("{label} constraint enforced");
    }

    let long_name = "n".repeat(121);
    assert_code(
        insert(
            &pool,
            admin_id,
            &format!("assets/{}.mp3", Uuid::now_v7()),
            "audio/mpeg",
            1,
            "en-GB",
            "female",
            &long_name,
        )
        .await,
        CHECK_VIOLATION,
    );

    insert(
        &pool,
        admin_id,
        "assets/duration.mp3",
        "audio/mpeg",
        1,
        "en-GB",
        "female",
        "a.mp3",
    )
    .await
    .unwrap();
    assert_code(
        sqlx::query("UPDATE lexicon.audio_assets SET duration_ms = 0 WHERE object_key = $1")
            .bind("assets/duration.mp3")
            .execute(&pool)
            .await,
        CHECK_VIOLATION,
    );
}

#[sqlx::test]
async fn audio_assets_require_an_existing_admin(pool: PgPool) {
    assert_code(
        insert(
            &pool,
            Uuid::now_v7(),
            "assets/orphan.mp3",
            "audio/mpeg",
            1,
            "en-GB",
            "female",
            "a.mp3",
        )
        .await,
        FOREIGN_KEY_VIOLATION,
    );
}
