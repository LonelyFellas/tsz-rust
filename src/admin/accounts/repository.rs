use sqlx::PgPool;
use uuid::Uuid;

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

    #[error("admin not found")]
    NotFound,

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

    /// 按 id 取治理视图的一行（含创建者名，与列表同形状）。
    /// 治理端点用它做「存在性 + 目标角色」判定，判完再写——role 在本系统里不可变
    /// （无提级/降级端点，见设计 §0 非目标），故读后写没有角色漂移的窗口。
    pub(crate) async fn find_by_id(
        &self,
        id: &Uuid,
    ) -> Result<Option<AdminAccountRecord>, AdminAccountsRepositoryError> {
        sqlx::query_as!(
            AdminAccountRecord,
            r#"
            SELECT
                a.id AS "id!",
                a.phone AS "phone!",
                a.display_name AS "display_name!",
                a.role as "role!: AdminRole",
                a.status as "status!: AdminStatus",
                creator.id AS "created_by_id?",
                creator.display_name AS "created_by_display_name?",
                a.created_at AS "created_at!",
                a.updated_at AS "updated_at!"
            FROM admins a
            LEFT JOIN admins creator
                ON creator.id = a.created_by_admin_id
            WHERE a.id = $1
            "#,
            id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(AdminAccountsRepositoryError::Database)
    }

    /// 启禁用：更新 status 并回读治理视图（含创建者名），让调用方回显的一定是落库事实。
    /// CTE 保证「改」与「读」在同一条语句里，中间没有别人插队改写的窗口。
    pub(crate) async fn set_status(
        &self,
        id: &Uuid,
        status: AdminStatus,
    ) -> Result<AdminAccountRecord, AdminAccountsRepositoryError> {
        sqlx::query_as!(
            AdminAccountRecord,
            r#"
            WITH updated AS (
                UPDATE admins
                SET status = $2, updated_at = NOW()
                WHERE id = $1
                RETURNING id, phone, display_name, role, status, created_by_admin_id,
                          created_at, updated_at
            )
            SELECT
                u.id AS "id!",
                u.phone AS "phone!",
                u.display_name AS "display_name!",
                u.role as "role!: AdminRole",
                u.status as "status!: AdminStatus",
                creator.id AS "created_by_id?",
                creator.display_name AS "created_by_display_name?",
                u.created_at AS "created_at!",
                u.updated_at AS "updated_at!"
            FROM updated u
            LEFT JOIN admins creator
                ON creator.id = u.created_by_admin_id
            "#,
            id,
            status as AdminStatus,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(AdminAccountsRepositoryError::Database)?
        .ok_or(AdminAccountsRepositoryError::NotFound)
    }

    /// 超管重置普通管理员密码：写新哈希并**强制**置 `must_change_password`。
    /// 该标志不是调用方的选项——重置出来的临时密码必须被改掉（设计 §7），
    /// 所以写死在 SQL 里，不做成参数。
    pub(crate) async fn reset_password(
        &self,
        id: &Uuid,
        password_hash: &str,
    ) -> Result<(), AdminAccountsRepositoryError> {
        let result = sqlx::query!(
            r#"
            UPDATE admins
            SET password_hash = $2, must_change_password = TRUE, updated_at = NOW()
            WHERE id = $1
            "#,
            id,
            password_hash,
        )
        .execute(&self.pool)
        .await
        .map_err(AdminAccountsRepositoryError::Database)?;

        if result.rows_affected() == 0 {
            return Err(AdminAccountsRepositoryError::NotFound);
        }
        Ok(())
    }
}

fn map_create_error(error: sqlx::Error) -> AdminAccountsRepositoryError {
    if is_unique_violation(&error, "admins_phone_key") {
        AdminAccountsRepositoryError::AlreadyExists
    } else {
        AdminAccountsRepositoryError::Database(error)
    }
}
