mod coins_support;
use coins_support::*;
use sqlx::PgPool;
use tsz_rust::coins::model::OwnerType;
use uuid::Uuid;

const UP: &str = include_str!("../migrations/20261006030000_invitations.up.sql");
const DOWN: &str = include_str!("../migrations/20261006030000_invitations.down.sql");

#[sqlx::test]
async fn immutable_attribution_and_reward_shape(pool: PgPool) {
    let inviter = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let invitee = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    sqlx::query("INSERT INTO invitation_codes(user_id,code) VALUES($1,'0123456789ABCDEF')")
        .bind(inviter.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    let receipt = credit(&pool, inviter, 17, "invitation-schema").await;
    sqlx::query("INSERT INTO invitation_registrations(invitee_user_id,inviter_user_id,reward_status,reward_amount,operation_id) VALUES($1,$2,'awarded',17,$3)")
        .bind(invitee.owner_id).bind(inviter.owner_id).bind(receipt.operation_id)
        .execute(&pool).await.unwrap();
    for statement in [
        "INSERT INTO invitation_registrations SELECT * FROM invitation_registrations",
        "UPDATE invitation_registrations SET inviter_user_id=gen_random_uuid()",
        "UPDATE invitation_registrations SET reward_status='reward_disabled',reward_amount=0,operation_id=NULL",
        "DELETE FROM invitation_registrations",
        "UPDATE invitation_codes SET code='FEDCBA9876543210'",
        "DELETE FROM invitation_codes",
        "INSERT INTO invitation_codes VALUES(gen_random_uuid(),'0123456789ABCDEF',clock_timestamp())",
        "INSERT INTO invitation_codes VALUES(gen_random_uuid(),'invalid',clock_timestamp())",
        "INSERT INTO invitation_registrations SELECT inviter_user_id,inviter_user_id,'reward_disabled',0,NULL,clock_timestamp() FROM invitation_registrations",
        "INSERT INTO invitation_registrations SELECT gen_random_uuid(),inviter_user_id,'reward_disabled',1,NULL,clock_timestamp() FROM invitation_registrations",
        "INSERT INTO invitation_registrations SELECT gen_random_uuid(),inviter_user_id,'awarded',1,NULL,clock_timestamp() FROM invitation_registrations",
    ] {
        assert!(
            sqlx::query(statement).execute(&pool).await.is_err(),
            "{statement}"
        );
    }
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![inviter.owner_id, invitee.owner_id])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM invitation_registrations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(balance(&pool, inviter).await, "17");
    reconciled(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    assert!(sqlx::raw_sql(DOWN).execute(&mut *tx).await.is_err());
    tx.rollback().await.unwrap();
}

#[sqlx::test]
async fn empty_roundtrip_but_issued_codes_prevent_down(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(DOWN).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(UP).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let inviter = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    sqlx::query("INSERT INTO invitation_codes(user_id,code) VALUES($1,'0123456789ABCDEF')")
        .bind(inviter.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(sqlx::raw_sql(DOWN).execute(&mut *tx).await.is_err());
    tx.rollback().await.unwrap();
    sqlx::query("INSERT INTO invitation_registrations(invitee_user_id,inviter_user_id,reward_status,reward_amount) VALUES($1,$2,'reward_disabled',0)")
        .bind(Uuid::now_v7()).bind(inviter.owner_id).execute(&pool).await.unwrap();
    assert_eq!(balance(&pool, inviter).await, "0");
    reconciled(&pool).await;
}
