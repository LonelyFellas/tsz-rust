mod account_deletion_support;
mod wordlists_support;
use sqlx::PgPool;
const UP: &str = include_str!("../migrations/20261006040000_wordlists.up.sql");
const DOWN: &str = include_str!("../migrations/20261006040000_wordlists.down.sql");
#[sqlx::test]
async fn empty_roundtrip_and_content_prevents_destructive_down(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261018120000_wordlist_custom_items.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261018070000_wordlist_deletion_grace.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261016090000_wordlist_library.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261015020000_wordlist_notifications.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261015010000_wordlist_interactions.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261013160000_wordlist_author_notes.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261012120000_wordlist_management.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
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
    sqlx::raw_sql(include_str!(
        "../migrations/20261012120000_wordlist_management.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261013160000_wordlist_author_notes.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261015010000_wordlist_interactions.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261015020000_wordlist_notifications.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261016090000_wordlist_library.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261018070000_wordlist_deletion_grace.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261018120000_wordlist_custom_items.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("SET LOCAL enable_seqscan=off")
        .execute(&mut *tx)
        .await
        .unwrap();
    let plan: serde_json::Value = sqlx::query_scalar("EXPLAIN (FORMAT JSON) SELECT COALESCE(sum(amount),0)::text FROM wordlist_tips WHERE wordlist_id=$1")
        .bind(uuid::Uuid::now_v7()).fetch_one(&mut *tx).await.unwrap();
    assert!(
        plan.to_string().contains("wordlist_tips_wordlist"),
        "public statistics must have a usable wordlist index: {plan}"
    );
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

#[sqlx::test]
async fn deletion_events_keep_grace_and_notification_kind(pool: PgPool) {
    let (_, auth) = account_deletion_support::setup_bound(&pool).await;
    sqlx::query("INSERT INTO user_notifications(id,user_id,kind) VALUES($1,$2,'wordlist_deleted')")
        .bind(uuid::Uuid::now_v7())
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query(
            "INSERT INTO wordlist_deletion_events(id,wordlist_id,deleted_at,grace_until,prior_state,public_generation) VALUES($1,$2,clock_timestamp(),clock_timestamp(),'published',0)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(uuid::Uuid::now_v7())
        .execute(&pool)
        .await
        .is_err(),
        "grace must end after deletion"
    );
    sqlx::query(
        "INSERT INTO wordlist_deletion_events(id,wordlist_id,deleted_at,grace_until,prior_state,public_generation) VALUES($1,$2,clock_timestamp(),clock_timestamp()+interval '7 days','published',0)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(uuid::Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn deletion_grace_down_refuses_delivered_notifications(pool: PgPool) {
    let (_, auth) = account_deletion_support::setup_bound(&pool).await;
    sqlx::query("INSERT INTO user_notifications(id,user_id,kind) VALUES($1,$2,'wordlist_deleted')")
        .bind(uuid::Uuid::now_v7())
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261018070000_wordlist_deletion_grace.down.sql"
        ))
        .execute(&mut *tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
}
