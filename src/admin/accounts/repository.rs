use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    admin::{
        Admin, AdminDialectPreference, AdminRole, AdminStatus, NewAdmin,
        accounts::model::{AdminAccountAdminListFilter, AdminAccountRecord},
        permissions::{LEGACY_PUBLICATION_KEYS, catalog},
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

    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

    // 无敏感权限时，搜索和计数只匹配昵称，避免用联系方式猜测用户身份。
    pub(crate) async fn user_list_without_sensitive(
        &self,
        filter: &crate::user::model::UserListFilter,
    ) -> Result<(Vec<crate::user::model::UserListRecord>, i64), AdminAccountsRepositoryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(AdminAccountsRepositoryError::Database)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(AdminAccountsRepositoryError::Database)?;
        let role = filter
            .role
            .as_ref()
            .map(crate::user::model::UserRole::as_str);
        let total = sqlx::query_scalar::<_, i64>(r#"
            SELECT COUNT(*) FROM users u
            WHERE ($1::text IS NULL OR EXISTS(SELECT 1 FROM user_roles r WHERE r.user_id = u.id AND r.role = $1))
              AND ($2::text IS NULL OR u.display_name ILIKE $2 ESCAPE '\')
              AND ($3::timestamptz IS NULL OR u.created_at >= $3)
              AND ($4::timestamptz IS NULL OR u.created_at < $4)
        "#).bind(role).bind(&filter.query_pattern).bind(filter.registered_from).bind(filter.registered_to)
            .fetch_one(&mut *tx).await.map_err(AdminAccountsRepositoryError::Database)?;
        let records = sqlx::query_as::<_, crate::user::model::UserListRecord>(r#"
            SELECT u.id, NULL::text AS phone, NULL::text AS email, u.display_name, u.avatar_url, u.status,
                ARRAY(SELECT r.role FROM user_roles r WHERE r.user_id = u.id ORDER BY CASE r.role WHEN 'student' THEN 1 ELSE 2 END) AS roles,
                EXISTS(SELECT 1 FROM teacher_profiles p WHERE p.user_id = u.id AND p.verified) AS teacher_verified,
                u.created_at, u.updated_at
            FROM users u
            WHERE ($1::text IS NULL OR EXISTS(SELECT 1 FROM user_roles r WHERE r.user_id = u.id AND r.role = $1))
              AND ($2::text IS NULL OR u.display_name ILIKE $2 ESCAPE '\')
              AND ($3::timestamptz IS NULL OR u.created_at >= $3)
              AND ($4::timestamptz IS NULL OR u.created_at < $4)
            ORDER BY u.created_at DESC, u.id DESC LIMIT $5 OFFSET $6
        "#).bind(role).bind(&filter.query_pattern).bind(filter.registered_from).bind(filter.registered_to)
            .bind(filter.limit).bind(filter.offset).fetch_all(&mut *tx).await.map_err(AdminAccountsRepositoryError::Database)?;
        tx.commit()
            .await
            .map_err(AdminAccountsRepositoryError::Database)?;
        Ok((records, total))
    }

    pub(crate) async fn update_user(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        id: &Uuid,
        status: Option<crate::user::model::UserStatus>,
        display_name: Option<&str>,
    ) -> Result<Option<crate::user::model::UserListRecord>, AdminAccountsRepositoryError> {
        sqlx::query_as::<_, crate::user::model::UserListRecord>(r#"
            WITH updated AS (
                UPDATE users SET status = COALESCE($2::text, status),
                    display_name = COALESCE($3::text, display_name), updated_at = NOW()
                WHERE id = $1
                RETURNING id, phone, email, display_name, avatar_url, status, created_at, updated_at
            )
            SELECT u.*,
                ARRAY(SELECT r.role FROM user_roles r WHERE r.user_id = u.id ORDER BY CASE r.role WHEN 'student' THEN 1 ELSE 2 END) AS roles,
                EXISTS(SELECT 1 FROM teacher_profiles p WHERE p.user_id = u.id AND p.verified) AS teacher_verified
            FROM updated u
        "#).bind(id).bind(status.map(|value| value.as_str())).bind(display_name)
            .fetch_optional(&mut **tx).await.map_err(AdminAccountsRepositoryError::Database)
    }

    pub(crate) async fn update_display_name(
        &self,
        id: &Uuid,
        display_name: &str,
    ) -> Result<AdminAccountRecord, AdminAccountsRepositoryError> {
        let result =
            sqlx::query("UPDATE admins SET display_name = $2, updated_at = NOW() WHERE id = $1")
                .bind(id)
                .bind(display_name)
                .execute(&self.pool)
                .await
                .map_err(AdminAccountsRepositoryError::Database)?;
        if result.rows_affected() == 0 {
            return Err(AdminAccountsRepositoryError::NotFound);
        }
        self.find_by_id(id)
            .await?
            .ok_or(AdminAccountsRepositoryError::NotFound)
    }

    pub async fn create(&self, admin: NewAdmin) -> Result<Admin, AdminAccountsRepositoryError> {
        sqlx::query_as!(
            Admin,
            r#"
            INSERT INTO admins (id, phone, display_name, password_hash, role, must_change_password, created_by_admin_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id, phone, display_name, password_hash, security_version,
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
                (a.role = 'super_admin' OR (
                    SELECT COUNT(*) FROM admin_permission_grants g
                    WHERE g.admin_id = a.id AND g.permission_key = ANY($6::text[])
                ) = cardinality($6::text[])) AS can_publish_lexicon,
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
        .bind(legacy_publication_required_keys())
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
        let required_keys = legacy_publication_required_keys();
        sqlx::query_as!(
            AdminAccountRecord,
            r#"
            SELECT
                a.id AS "id!",
                a.phone AS "phone!",
                a.display_name AS "display_name!",
                a.role as "role!: AdminRole",
                a.status as "status!: AdminStatus",
                (a.role = 'super_admin' OR (
                    SELECT COUNT(*) FROM admin_permission_grants g
                    WHERE g.admin_id = a.id AND g.permission_key = ANY($2::text[])
                ) = cardinality($2::text[])) AS "can_publish_lexicon!",
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
            &required_keys,
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
        let required_keys = legacy_publication_required_keys();
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
                (u.role = 'super_admin' OR (
                    SELECT COUNT(*) FROM admin_permission_grants g
                    WHERE g.admin_id = u.id AND g.permission_key = ANY($3::text[])
                ) = cardinality($3::text[])) AS "can_publish_lexicon!",
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
            &required_keys,
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
        crate::admin::AdminRepository::new(self.pool.clone())
            .set_password(id, password_hash, true)
            .await
            .map_err(|error| match error {
                crate::admin::AdminRepositoryError::NotFound => {
                    AdminAccountsRepositoryError::NotFound
                }
                crate::admin::AdminRepositoryError::Db(error) => {
                    AdminAccountsRepositoryError::Database(error)
                }
                other => {
                    AdminAccountsRepositoryError::Database(sqlx::Error::Protocol(other.to_string()))
                }
            })
    }
}

fn legacy_publication_required_keys() -> Vec<String> {
    let mut keys = LEGACY_PUBLICATION_KEYS
        .iter()
        .map(|key| (*key).to_owned())
        .collect();
    catalog::expand_grants(&mut keys);
    keys.into_iter().collect()
}

fn map_create_error(error: sqlx::Error) -> AdminAccountsRepositoryError {
    if is_unique_violation(&error, "admins_phone_key") {
        AdminAccountsRepositoryError::AlreadyExists
    } else {
        AdminAccountsRepositoryError::Database(error)
    }
}
