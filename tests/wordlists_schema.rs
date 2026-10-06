mod account_deletion_support;
mod wordlists_support;
use sqlx::PgPool;
const UP: &str = include_str!("../migrations/20261006040000_wordlists.up.sql");
const DOWN: &str = include_str!("../migrations/20261006040000_wordlists.down.sql");
#[sqlx::test]
async fn empty_roundtrip_and_content_prevents_destructive_down(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261006050000_wordlist_tips.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(DOWN).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(UP).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261006050000_wordlist_tips.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (state, auth) = account_deletion_support::setup_bound(&pool).await;
    let id = wordlists_support::entry(&pool, "schema").await;
    let created = account_deletion_support::call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        wordlists_support::create_body(&[id]),
    )
    .await;
    assert_eq!(created.0, axum::http::StatusCode::OK, "{}", created.1);
    let mut tx = pool.begin().await.unwrap();
    assert!(sqlx::raw_sql(DOWN).execute(&mut *tx).await.is_err());
    tx.rollback().await.unwrap();
    assert!(
        sqlx::query("UPDATE wordlists SET owner_user_id=gen_random_uuid()")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("INSERT INTO wordlist_items SELECT * FROM wordlist_items")
            .execute(&pool)
            .await
            .is_err()
    );
}
