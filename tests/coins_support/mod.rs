use sqlx::PgPool;
use tsz_rust::coins::{model::*, repository, service};
use uuid::Uuid;

pub async fn seed(pool: &PgPool, owner_type: OwnerType, id: Uuid) -> Owner {
    let query = match owner_type {
        OwnerType::User => {
            "INSERT INTO users (id,email,password_hash,display_name) VALUES ($1,$2,'hash','Coin user')"
        }
        OwnerType::Admin => {
            "INSERT INTO admins (id,phone,password_hash,display_name,role,must_change_password) VALUES ($1,$2,'hash','Coin admin','admin',false)"
        }
    };
    sqlx::query(query)
        .bind(id)
        .bind(format!("{id}@example.test"))
        .execute(pool)
        .await
        .unwrap();
    Owner {
        owner_type,
        owner_id: id,
    }
}
pub fn context(key: &str) -> Context {
    Context {
        actor: Actor::System,
        idempotency_scope: "test".into(),
        idempotency_key: key.into(),
        source_type: "test_reward".into(),
        source_id: key.into(),
        reason: "private reason".into(),
        evidence_ref: Some("private evidence".into()),
    }
}
pub async fn credit(pool: &PgPool, owner: Owner, value: i64, key: &str) -> Receipt {
    let mut tx = pool.begin().await.unwrap();
    let receipt = service::credit_in(&mut tx, owner, Amount::new(value).unwrap(), &context(key))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    receipt
}
pub async fn balance(pool: &PgPool, owner: Owner) -> String {
    repository::wallet(pool, owner).await.unwrap().balance
}
pub async fn reconciled(pool: &PgPool) {
    let report = repository::reconcile(pool).await.unwrap();
    assert_eq!(report.wallet_mismatches, 0, "{report:?}");
    assert_eq!(report.running_balance_mismatches, 0, "{report:?}");
    assert_eq!(report.operation_mismatches, 0, "{report:?}");
    assert_eq!(report.total_balance, report.net_issuance);
}
