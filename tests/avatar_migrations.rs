use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn avatar_owner_constraints_survive_hard_delete(pool: PgPool) {
    let owner = Uuid::now_v7();
    let other = Uuid::now_v7();
    for id in [owner, other] {
        sqlx::query("INSERT INTO users (id,email,password_hash,display_name,avatar_url) VALUES ($1,$2,'hash','Avatar','https://legacy.example/avatar')")
            .bind(id).bind(format!("{id}@example.test")).execute(&pool).await.unwrap();
    }
    let upload = Uuid::now_v7();
    sqlx::query("INSERT INTO avatar_uploads (id,user_id,source_key,declared_type,size_bytes,expires_at) VALUES ($1,$2,$3,'image/png',10,now()+interval '10 minutes')")
        .bind(upload).bind(owner).bind(format!("uploads/avatars/{upload}/original.png")).execute(&pool).await.unwrap();
    let error = sqlx::query("UPDATE users SET avatar_upload_id=$1 WHERE id=$2")
        .bind(upload)
        .bind(other)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23503")
    );
    sqlx::query("UPDATE users SET avatar_upload_id=$1 WHERE id=$2")
        .bind(upload)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO avatar_cleanup_tasks (id,upload_id,object_key,kind,not_before) VALUES ($1,$2,$3,'source',now())").bind(Uuid::now_v7()).bind(upload).bind(format!("uploads/avatars/{upload}/original.png")).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let owner_after: Option<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM avatar_uploads WHERE id=$1")
            .bind(upload)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(owner_after, None);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM avatar_cleanup_tasks WHERE upload_id=$1")
            .bind(upload)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let legacy: String = sqlx::query_scalar("SELECT avatar_url FROM users WHERE id=$1")
        .bind(other)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(legacy, "https://legacy.example/avatar");
}

#[sqlx::test]
async fn avatar_up_down_preserves_legacy_urls_and_clears_only_owned_references(pool: PgPool) {
    let legacy = Uuid::now_v7();
    let current = Uuid::now_v7();
    let upload = Uuid::now_v7();
    for id in [legacy, current] {
        sqlx::query("INSERT INTO users (id,email,password_hash,display_name,avatar_url) VALUES ($1,$2,'hash','Avatar','https://legacy.example/avatar')").bind(id).bind(format!("{id}@example.test")).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO avatar_uploads (id,user_id,source_key,declared_type,size_bytes,expires_at,state,canonical_key,confirmed_at) VALUES ($1,$2,'uploads/test','image/png',10,now(),'confirmed','images/test',now())").bind(upload).bind(current).execute(&pool).await.unwrap();
    sqlx::query("UPDATE users SET avatar_upload_id=$1,avatar_url='https://api.example/avatars/new' WHERE id=$2").bind(upload).bind(current).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261002000000_avatar_uploads.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let values: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id,avatar_url FROM users WHERE id=ANY($1)")
            .bind(vec![legacy, current])
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(values.contains(&(legacy, "https://legacy.example/avatar".into())));
    assert!(values.contains(&(current, String::new())));
    sqlx::raw_sql(include_str!(
        "../migrations/20261002000000_avatar_uploads.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM avatar_uploads")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let refs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM users WHERE avatar_upload_id IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(refs, 0);
}

#[sqlx::test]
async fn avatar_checks_reject_invalid_metadata(pool: PgPool) {
    for (kind, size) in [("image/gif", 1), ("image/png", 0), ("image/png", 5242881)] {
        let error=sqlx::query("INSERT INTO avatar_uploads (id,source_key,declared_type,size_bytes,expires_at) VALUES ($1,$2,$3,$4,now())").bind(Uuid::now_v7()).bind(Uuid::now_v7().to_string()).bind(kind).bind(size as i64).execute(&pool).await.unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("23514")
        );
    }
    let error=sqlx::query("INSERT INTO avatar_uploads (id,source_key,declared_type,size_bytes,expires_at,state) VALUES ($1,$2,'image/png',1,now(),'confirmed')").bind(Uuid::now_v7()).bind(Uuid::now_v7().to_string()).execute(&pool).await.unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23514")
    );
}
