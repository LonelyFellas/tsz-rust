use anyhow::Context;
use tsz_rust::admin::{AdminRepository, AdminService, SeedOutcome};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let db_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let phone = std::env::var("SEED_ADMIN_PHONE").context("SEED_ADMIN_PHONE is required")?;
    let password =
        std::env::var("SEED_ADMIN_PASSWORD").context("SEED_ADMIN_PASSWORD is required")?;
    let display_name =
        std::env::var("SEED_ADMIN_DISPLAY_NAME").unwrap_or_else(|_| "Administrator".into());

    let pool = tsz_rust::platform::connect_db(&db_url).await?;
    // 空库直接 seed 会因表不存在失败，所以这里也跑迁移。
    // 迁移幂等且持 Postgres advisory lock，与 server 的启动迁移重复执行无害。
    sqlx::migrate!("./migrations").run(&pool).await?;

    let svc = AdminService::new(AdminRepository::new(pool));
    match svc
        .seed_super_admin(&phone, &password, &display_name)
        .await?
    {
        SeedOutcome::Created(a) => tracing::info!(id = %a.id, "super admin created"),
        SeedOutcome::Unchanged(a) => tracing::info!(id = %a.id, "super admin already ok"),
    }
    Ok(())
}
