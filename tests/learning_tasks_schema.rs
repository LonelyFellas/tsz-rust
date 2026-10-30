use sqlx::PgPool;
#[sqlx::test]
async fn wordlist_practice_migration_roundtrip_refuses_custom_fact_loss(pool: PgPool) {
    let down = include_str!("../migrations/20261030040000_wordlist_practice.down.sql");
    let up = include_str!("../migrations/20261030040000_wordlist_practice.up.sql");
    sqlx::raw_sql(down).execute(&pool).await.unwrap();
    sqlx::raw_sql(up).execute(&pool).await.unwrap();
    let nullable: i64=sqlx::query_scalar("SELECT count(*) FROM information_schema.columns WHERE table_name IN ('learning_questions','learning_cycle_units') AND column_name='entry_id' AND is_nullable='YES'").fetch_one(&pool).await.unwrap();
    assert_eq!(nullable, 2);
    let user = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users(id,email,password_hash,display_name) VALUES($1,$2,'hash','schema')",
    )
    .bind(user)
    .bind(format!("{user}@example.test"))
    .execute(&pool)
    .await
    .unwrap();
    let task = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO learning_tasks(id,user_id,name,task_type,wordlist_ids,create_key,create_hash) VALUES($1,$2,'schema','longterm',ARRAY[$3]::uuid[],$4,'hash')").bind(task).bind(user).bind(uuid::Uuid::now_v7()).bind(uuid::Uuid::now_v7()).execute(&pool).await.unwrap();
    let cycle = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO learning_cycles(id,task_id,user_id,cycle_number,state,total_count,settings_snapshot,generation_version,grading_version,earliest_day) VALUES($1,$2,$3,1,'active',1,'{}','wordlist_all_v1','spelling_exact_v1',CURRENT_DATE)").bind(cycle).bind(task).bind(user).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO learning_cycle_units(id,cycle_id,position,unit_key,source_wordlist_id,source_revision,source_membership_id,custom_item_id,form_ids,prompt_snapshot,answer_snapshot,fingerprint) VALUES($1,$2,0,'custom',$3,1,$4,$5,'{}','{}','{}','hash')").bind(uuid::Uuid::now_v7()).bind(cycle).bind(uuid::Uuid::now_v7()).bind(uuid::Uuid::now_v7()).bind(uuid::Uuid::now_v7()).execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(sqlx::raw_sql(down).execute(&mut *tx).await.is_err());
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM learning_cycle_units WHERE custom_item_id IS NOT NULL"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
}
#[sqlx::test]
async fn empty_down_up_roundtrip(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../migrations/20261018130000_learning_coverage.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
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

#[sqlx::test]
async fn coverage_migration_preserves_legacy_and_rejects_partial_schedule(pool: PgPool) {
    let user = uuid::Uuid::now_v7();
    let task = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users(id,email,password_hash,display_name) VALUES($1,$2,'hash','legacy')",
    )
    .bind(user)
    .bind(format!("{user}@example.test"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO learning_tasks(id,user_id,name,task_type,wordlist_ids,daily_question_count,create_key,create_hash) VALUES($1,$2,'legacy','daily',ARRAY[$3]::uuid[],1,$4,'hash')").bind(task).bind(user).bind(uuid::Uuid::now_v7()).bind(uuid::Uuid::now_v7()).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261018130000_learning_coverage.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261018130000_learning_coverage.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let mode: String =
        sqlx::query_scalar("SELECT generation_policy FROM learning_tasks WHERE id=$1")
            .bind(task)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(mode, "legacy_v1");
    assert!(sqlx::query("UPDATE learning_tasks SET generation_policy='coverage_v1',start_date=current_date WHERE id=$1").bind(task).execute(&pool).await.is_err());
    let cycles: i64 = sqlx::query_scalar("SELECT count(*) FROM learning_cycles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cycles, 0);
}
