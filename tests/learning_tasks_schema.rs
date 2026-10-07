use sqlx::PgPool;
#[sqlx::test]
async fn empty_down_up_roundtrip(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../migrations/20261007020000_learning_rewards.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261007010000_learning_tasks.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261007010000_learning_tasks.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let tables:i64=sqlx::query_scalar("SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_name IN ('learning_tasks','learning_runs','learning_questions','learning_answers','learning_completions','learning_run_start_requests')").fetch_one(&pool).await.unwrap();
    assert_eq!(tables, 6);
}

#[sqlx::test]
async fn daily_requires_count_and_facts_block_down(pool: PgPool) {
    let user = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users(id,email,password_hash,display_name) VALUES($1,$2,'hash','schema')",
    )
    .bind(user)
    .bind(format!("{user}@example.test"))
    .execute(&pool)
    .await
    .unwrap();
    let insert = "INSERT INTO learning_tasks(id,user_id,name,task_type,wordlist_ids,daily_question_count,create_key,create_hash) VALUES($1,$2,'test','daily',ARRAY[$3]::uuid[],$4,$5,'hash')";
    assert!(
        sqlx::query(insert)
            .bind(uuid::Uuid::now_v7())
            .bind(user)
            .bind(uuid::Uuid::now_v7())
            .bind(None::<i32>)
            .bind(uuid::Uuid::now_v7())
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query(insert)
        .bind(uuid::Uuid::now_v7())
        .bind(user)
        .bind(uuid::Uuid::now_v7())
        .bind(Some(1))
        .bind(uuid::Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261007010000_learning_tasks.down.sql"
        ))
        .execute(&mut *tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
}
