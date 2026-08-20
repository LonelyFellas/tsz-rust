use sqlx::PgPool;

use crate::{
    admin::{
        Admin, AdminDialectPreference, AdminRole, AdminStatus, NewAdmin,
        accounts::model::{AdminAccountAdminListFilter, AdminAccountRecord},
    },
    platform::is_unique_violation,
};

#[derive(Debug, thiserror::Error)]
pub enum AdminAccountsRepositoryError {
    #[error("admin phone already exists")]
    AlreadyExists,

    #[error("database operation failed")]
    Database(#[source] sqlx::Error),
}

pub struct AdminAccountsRepository {
    pool: PgPool,
}

impl AdminAccountsRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, admin: NewAdmin) -> Result<Admin, AdminAccountsRepositoryError> {
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
                      dialect_preference as "dialect_preference: AdminDialectPreference",
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
        .map_err(map_create_error)
    }

    pub(crate) async fn admin_list(
        &self,
        filter: &AdminAccountAdminListFilter,
    ) -> Result<(Vec<AdminAccountRecord>, i64), AdminAccountsRepositoryError> {
        // 创建一个事务
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(AdminAccountsRepositoryError::Database)?;

        // 使用同一个只读事务
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(AdminAccountsRepositoryError::Database)?;

        // 查询总数
        let role = filter.role.as_ref().map(AdminRole::as_str);
        let phone_pattern = filter.phone_pattern.as_deref();
        let display_name_pattern = filter.display_name_pattern.as_deref();

        let total = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*)
            FROM admins a
            WHERE ($1::text IS NULL OR a.role = $1)
              AND ($2::text IS NULL OR a.phone ILIKE $2 ESCAPE '\')
              AND ($3::text IS NULL OR a.display_name ILIKE $3 ESCAPE '\')
            "#,
        )
        .bind(role)
        .bind(phone_pattern)
        .bind(display_name_pattern)
        .fetch_one(&mut *tx)
        .await
        .map_err(AdminAccountsRepositoryError::Database)?;

        // 查询当前页
        let records = sqlx::query_as::<_, AdminAccountRecord>(
            r#"
            SELECT
                a.id,
                a.phone,
                a.display_name,
                a.role,
                a.status,
                creator.id AS created_by_id,
                creator.display_name AS created_by_display_name,
                a.created_at,
                a.updated_at
            FROM admins a
            LEFT JOIN admins creator
                ON creator.id = a.created_by_admin_id
            WHERE ($1::text IS NULL OR a.role = $1)
              AND ($2::text IS NULL OR a.phone ILIKE $2 ESCAPE '\')
              AND ($3::text IS NULL OR a.display_name ILIKE $3 ESCAPE '\')
            ORDER BY a.created_at DESC, a.id DESC
            LIMIT $4 OFFSET $5
            "#,
        )
        .bind(role)
        .bind(phone_pattern)
        .bind(display_name_pattern)
        .bind(filter.limit)
        .bind(filter.offset)
        .fetch_all(&mut *tx)
        .await
        .map_err(AdminAccountsRepositoryError::Database)?;

        tx.commit()
            .await
            .map_err(AdminAccountsRepositoryError::Database)?;

        Ok((records, total))
    }
}

fn map_create_error(error: sqlx::Error) -> AdminAccountsRepositoryError {
    if is_unique_violation(&error, "admins_phone_key") {
        AdminAccountsRepositoryError::AlreadyExists
    } else {
        AdminAccountsRepositoryError::Database(error)
    }
}
