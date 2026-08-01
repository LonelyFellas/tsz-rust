use sqlx::PgPool;
use tsz_rust::admin::{
    AdminRole, NewAdmin,
    accounts::{AdminAccountsRepository, AdminAccountsRepositoryError},
};
use uuid::Uuid;

fn new_admin(id: Uuid, phone: &str) -> NewAdmin {
    NewAdmin {
        id,
        phone: phone.to_owned(),
        display_name: "运营管理员".to_owned(),
        password_hash: "$2b$12$abcdefghijklmnopqrstuuuuuuuuuuuuuuuuuuuuuuuuuuuu".to_owned(),
        role: AdminRole::Admin,
        must_change_password: true,
        created_by_admin_id: None,
    }
}

#[sqlx::test]
async fn duplicate_phone_maps_to_repository_conflict(pool: PgPool) {
    let repository = AdminAccountsRepository::new(pool);
    let phone = "13800138000";

    repository
        .create(new_admin(Uuid::now_v7(), phone))
        .await
        .unwrap();

    let error = repository
        .create(new_admin(Uuid::now_v7(), phone))
        .await
        .unwrap_err();

    assert!(matches!(error, AdminAccountsRepositoryError::AlreadyExists));
}
