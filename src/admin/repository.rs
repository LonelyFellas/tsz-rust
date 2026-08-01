use chrono::Duration;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    admin::{Admin, AdminRole, AdminStatus, NewAdmin},
    platform::is_unique_violation,
};

#[derive(Debug, thiserror::Error)]
pub enum AdminRepositoryError {
    #[error("admin not found")]
    NotFound,
    #[error("admin already exists")]
    AlreadyExists,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub struct AdminRepository {
    pool: PgPool,
}

impl AdminRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn create(&self, admin: NewAdmin) -> Result<Admin, AdminRepositoryError> {
        sqlx::query_as!(
            Admin,
            r#"
            INSERT INTO admins (id, phone, display_name, password_hash, role, must_change_password, created_by_admin_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id, phone, display_name, password_hash,
                      role as "role: AdminRole",
                      status as "status: AdminStatus",
                      must_change_password, failed_login_count, locked_until,
                      created_by_admin_id,
                      created_at, updated_at
            "#,
            admin.id,
            admin.phone,
            admin.display_name,
            admin.password_hash,
            admin.role as AdminRole,
            admin.must_change_password,
            admin.created_by_admin_id
        )
        .fetch_one(&self.pool)
        .await
        .map_err(map_unique_violation)
    }
    pub async fn get_by_id(&self, id: &Uuid) -> Result<Admin, AdminRepositoryError> {
        sqlx::query_as!(
            Admin,
            r#"
            SELECT id, phone, display_name, password_hash, role as "role: AdminRole", status as "status: AdminStatus", must_change_password, failed_login_count, locked_until, created_by_admin_id, created_at, updated_at
            FROM admins
            WHERE id = $1
            "#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AdminRepositoryError::NotFound)
    }

    pub async fn get_by_phone(&self, phone: &str) -> Result<Admin, AdminRepositoryError> {
        sqlx::query_as!(
            Admin,
            r#"
            SELECT id, phone, display_name, password_hash, role as "role: AdminRole", status as "status: AdminStatus", must_change_password, failed_login_count, locked_until, created_by_admin_id, created_at, updated_at
            FROM admins
            WHERE phone = $1
            "#,
            phone,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AdminRepositoryError::NotFound)
    }
    pub async fn get_roles_by_admin_id(
        &self,
        id: &Uuid,
    ) -> Result<AdminRole, AdminRepositoryError> {
        let role = sqlx::query_scalar!(
            r#"
            SELECT role as "role: AdminRole" FROM admins WHERE id = $1
            "#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AdminRepositoryError::NotFound)?;

        Ok(role)
    }

    // 强制重置等场景，无条件更新，比如超级管理员给管理员重置密码等情况
    pub async fn set_password(
        &self,
        id: &Uuid,
        password_hash: &str,
        must_change_password: bool,
    ) -> Result<(), AdminRepositoryError> {
        let result = sqlx::query!(
            r#"
                   UPDATE admins
                   SET password_hash = $2, must_change_password = $3, updated_at = NOW()
                   WHERE id = $1
                   "#,
            id,
            password_hash,
            must_change_password,
        )
        .execute(&self.pool)
        .await?;

        ensure_found(result)
    }

    /// 用户自主改密，带旧哈希CAS
    pub async fn set_password_if_unchanged(
        &self,
        id: &Uuid,
        current_password_hash: &str,
        new_password_hash: &str,
        must_change_password: bool,
    ) -> Result<u64, AdminRepositoryError> {
        let result = sqlx::query!(
            r#"
            UPDATE admins
            SET password_hash = $2, must_change_password = $3, updated_at = NOW()
            WHERE id = $1 AND password_hash = $4
            "#,
            id,
            new_password_hash,
            must_change_password,
            current_password_hash
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn register_failed_login(
        &self,
        id: &Uuid,
        threshold: i32,
        lock_for: Duration,
    ) -> Result<bool, AdminRepositoryError> {
        sqlx::query_scalar!(
            r#"
            UPDATE admins
            SET failed_login_count = CASE
                    WHEN locked_until > NOW() THEN failed_login_count
                    WHEN failed_login_count + 1 >= $2 THEN 0
                    ELSE failed_login_count + 1
                END,
                locked_until = CASE
                    WHEN locked_until > NOW() THEN locked_until
                    WHEN failed_login_count + 1 >= $2 THEN $3
                    ELSE NULL
                END,
                updated_at = NOW()
            WHERE id = $1
            RETURNING (locked_until IS NOT NULL AND locked_until > NOW()) AS "locked!"
            "#,
            id,
            threshold,
            chrono::Utc::now() + lock_for,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AdminRepositoryError::NotFound)
    }
    pub async fn clear_failed_login(&self, id: &Uuid) -> Result<(), AdminRepositoryError> {
        let result = sqlx::query!(
            r#"
            UPDATE admins
            SET failed_login_count = 0, locked_until = NULL, updated_at = NOW()
            WHERE id = $1
            "#,
            id,
        )
        .execute(&self.pool)
        .await?;
        ensure_found(result)
    }
}
/// UPDATE 按 id 打空（0 行受影响）⇒ NotFound，杜绝幽灵成功。
fn ensure_found(result: sqlx::postgres::PgQueryResult) -> Result<(), AdminRepositoryError> {
    if result.rows_affected() == 0 {
        return Err(AdminRepositoryError::NotFound);
    }
    Ok(())
}

fn map_unique_violation(e: sqlx::Error) -> AdminRepositoryError {
    if is_unique_violation(&e, "admins_phone_key") {
        return AdminRepositoryError::AlreadyExists;
    }
    AdminRepositoryError::Db(e)
}
