use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn empty_roundtrip_and_policy_immutability(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../migrations/20261007020000_learning_rewards.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261007020000_learning_rewards.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let insert = "INSERT INTO learning_reward_policies(id,rule_version,effective_business_day,enabled,daily_amount,minimum_units) VALUES($1,$2,((clock_timestamp() AT TIME ZONE 'Asia/Shanghai')-interval '4 hours')::date+$3,true,$4,$5)";
    for (offset, amount, minimum) in [
        (0, Some(13_i64), Some(2_i32)),
        (1, None, Some(2)),
        (1, Some(0), Some(2)),
        (1, Some(13), Some(0)),
        (1, Some(13), Some(201)),
    ] {
        assert!(
            sqlx::query(insert)
                .bind(Uuid::now_v7())
                .bind(Uuid::now_v7().to_string())
                .bind(offset)
                .bind(amount)
                .bind(minimum)
                .execute(&pool)
                .await
                .is_err()
        );
    }
    let id = Uuid::now_v7();
    sqlx::query(insert)
        .bind(id)
        .bind("test-v1")
        .bind(1_i32)
        .bind(13_i64)
        .bind(2_i32)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE learning_reward_policies SET daily_amount=14 WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM learning_reward_policies WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .is_err()
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261007020000_learning_rewards.down.sql"
        ))
        .execute(&mut *tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
}

#[sqlx::test]
async fn settlement_shapes_and_daily_identity(pool: PgPool) {
    let insert = "INSERT INTO learning_reward_settlements(id,user_id,business_day,trigger_completion_id,completed_at,qualifying_units,status,awarded_amount,operation_id) VALUES($1,$2,CURRENT_DATE,$3,clock_timestamp(),0,$4,$5,NULL)";
    let user = Uuid::now_v7();
    for (status, amount) in [
        ("awarded", 13_i64),
        ("wallet_unavailable", 0),
        ("reward_disabled", 1),
    ] {
        assert!(
            sqlx::query(insert)
                .bind(Uuid::now_v7())
                .bind(user)
                .bind(Uuid::now_v7())
                .bind(status)
                .bind(amount)
                .execute(&pool)
                .await
                .is_err()
        );
    }
    sqlx::query(insert)
        .bind(Uuid::now_v7())
        .bind(user)
        .bind(Uuid::now_v7())
        .bind("reward_disabled")
        .bind(0_i64)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query(insert)
            .bind(Uuid::now_v7())
            .bind(user)
            .bind(Uuid::now_v7())
            .bind("reward_disabled")
            .bind(0_i64)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM learning_reward_settlements WHERE user_id=$1")
            .bind(user)
            .execute(&pool)
            .await
            .is_err()
    );
}
