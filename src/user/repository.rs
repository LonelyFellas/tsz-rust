use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    platform::{EmailError, PhoneError, is_unique_violation},
    user::model::{LearningSettings, User, UserListFilter, UserListRecord, UserRole, UserStatus},
};

#[derive(Debug, thiserror::Error)]
pub enum UserError {
    #[error(transparent)]
    Phone(#[from] PhoneError),
    #[error(transparent)]
    Email(#[from] EmailError),
    #[error("user not found")]
    NotFound,
    #[error("phone number already exists")]
    PhoneNumberAlreadyExists,
    #[error("email already exists")]
    EmailAlreadyExists,
    #[error("user already has this role")]
    AlreadyHasRole,
    #[error("missing subject")]
    MissingSubject,
    #[error("duplicate subject")]
    DuplicateSubject,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum SaveLearningSettingsError {
    #[error("invalid session")]
    InvalidSession,
    #[error("learning settings require a student profile")]
    StudentRequired,
    #[error("CEFR level cannot be changed")]
    LevelLocked,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[derive(Debug, PartialEq)]
pub struct NewUser {
    pub id: Uuid,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub password_hash: String,
    pub display_name: String,
    pub first_role: UserRole,
    /// 注册来源 IP，取自反代 X-Forwarded-For；反代没配这个头时为 None。
    pub registration_ip: Option<String>,
}

pub struct UserRepository {
    pool: PgPool,
}

impl UserRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    /// 创建用户
    pub async fn create(&self, input: NewUser) -> Result<User, UserError> {
        let mut tx = self.pool.begin().await?;
        let user = Self::create_in(&mut tx, input).await?;
        tx.commit().await?;
        Ok(user)
    }

    /// 在调用方提供的事务中创建用户及初始角色。
    /// 注册自动登录需要把这两步与 refresh token 落库一起提交。
    pub async fn create_in(
        connection: &mut PgConnection,
        input: NewUser,
    ) -> Result<User, UserError> {
        // 1) 插入users， RETURNING 拿回 DB 填的列
        let row = sqlx::query!(
            r#"
            INSERT INTO users (id, phone, email, password_hash, display_name, last_active_role, registration_ip)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING created_at, updated_at, status AS "status: UserStatus", avatar_url"#,
            input.id,
            input.phone,
            input.email,
            input.password_hash,
            input.display_name,
            input.first_role as UserRole,
            input.registration_ip
        )
        .fetch_one(&mut *connection)
        .await
        .map_err(map_unique_violation)?;

        // 2) 插入 user_roles
        sqlx::query!(
            r#"
            INSERT INTO user_roles (user_id, role)
            VALUES ($1, $2)
            "#,
            input.id,
            input.first_role as UserRole
        )
        .execute(&mut *connection)
        .await?;

        let user = User {
            id: input.id,
            phone: input.phone,
            email: input.email,
            password_hash: input.password_hash,
            security_version: 0,
            display_name: input.display_name,
            last_active_role: Some(input.first_role),
            status: UserStatus::Active,
            avatar_url: row.avatar_url,
            created_at: row.created_at,
            updated_at: row.updated_at,
        };

        Ok(user)
    }

    /// 通过手机号/邮箱进行查询用户
    pub async fn get_by_identifier(&self, identifier: &str) -> Result<User, UserError> {
        sqlx::query_as::<_, User>(
            "SELECT id, phone, email, password_hash, security_version, display_name, \
             last_active_role, created_at, updated_at, status, avatar_url \
             FROM users WHERE phone = $1 OR email = $1",
        )
        .bind(identifier)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(UserError::NotFound)
    }
    /// 通过user_id 查询用户
    pub async fn get_by_id(&self, id: &Uuid) -> Result<User, UserError> {
        let user = sqlx::query_as::<_, User>(
            "SELECT id, phone, email, password_hash, security_version, display_name, \
             last_active_role, created_at, updated_at, status, avatar_url FROM users WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(UserError::NotFound)?;

        Ok(user)
    }

    pub async fn learning_settings(
        &self,
        user_id: Uuid,
    ) -> Result<Option<LearningSettings>, UserError> {
        sqlx::query_as::<_, LearningSettings>(
            "SELECT cefr_level, english_variant FROM student_profiles \
             WHERE user_id = $1 AND cefr_level IS NOT NULL",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(UserError::Db)
    }

    pub async fn save_learning_settings(
        &self,
        user_id: Uuid,
        security_version: i64,
        settings: LearningSettings,
    ) -> Result<LearningSettings, SaveLearningSettingsError> {
        let mut tx = self.pool.begin().await?;
        // Lock the user because a first-time learner may not have a profile row yet.
        let user = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM users WHERE id = $1 AND security_version = $2 \
             AND status = 'active' FOR UPDATE",
        )
        .bind(user_id)
        .bind(security_version)
        .fetch_optional(&mut *tx)
        .await?;
        if user.is_none() || crate::account_deletion::is_effective_in(&mut tx, user_id).await? {
            return Err(SaveLearningSettingsError::InvalidSession);
        }
        let student = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM user_roles WHERE user_id = $1 AND role = 'student')",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        if !student {
            return Err(SaveLearningSettingsError::StudentRequired);
        }
        let current = sqlx::query_as::<_, LearningSettings>(
            "SELECT cefr_level, english_variant FROM student_profiles \
             WHERE user_id = $1 AND cefr_level IS NOT NULL",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        if current.is_some_and(|current| current.cefr_level != settings.cefr_level) {
            return Err(SaveLearningSettingsError::LevelLocked);
        }
        let saved = sqlx::query_as::<_, LearningSettings>(
            "INSERT INTO student_profiles (user_id, cefr_level, english_variant) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (user_id) DO UPDATE SET \
             cefr_level = EXCLUDED.cefr_level, english_variant = EXCLUDED.english_variant \
             RETURNING cefr_level, english_variant",
        )
        .bind(user_id)
        .bind(settings.cefr_level)
        .bind(settings.english_variant)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(saved)
    }

    pub async fn update_display_name(
        &self,
        user_id: Uuid,
        security_version: i64,
        display_name: &str,
    ) -> Result<Option<User>, UserError> {
        let mut tx = self.pool.begin().await?;
        let found:Option<Uuid>=sqlx::query_scalar("SELECT id FROM users WHERE id=$1 AND status='active' AND security_version=$2 FOR UPDATE").bind(user_id).bind(security_version).fetch_optional(&mut *tx).await?;
        if found.is_none() || crate::account_deletion::is_effective_in(&mut tx, user_id).await? {
            return Ok(None);
        }
        let result = sqlx::query_as::<_, User>(
            "UPDATE users SET display_name = $2, updated_at = NOW() \
             WHERE id = $1 AND status = 'active' AND security_version = $3 \
             RETURNING id, phone, email, password_hash, security_version, display_name, \
             last_active_role, status, avatar_url, created_at, updated_at",
        )
        .bind(user_id)
        .bind(display_name)
        .bind(security_version)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    /// 在同一事务内锁定用户、吊销其全部 refresh session，再删除用户。
    /// DELETE 的 FK cascade 会清理角色、profile 与 refresh token 行；显式 revoke
    /// 仍保留“先吊销、后删除”的安全顺序，避免未来 FK 策略变化破坏会话语义。
    pub async fn delete_account_in(
        connection: &mut PgConnection,
        user_id: Uuid,
    ) -> Result<bool, UserError> {
        let exists = sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE id = $1 FOR UPDATE")
            .bind(user_id)
            .fetch_optional(&mut *connection)
            .await?;
        if exists.is_none() {
            return Ok(false);
        }

        sqlx::query(
            "UPDATE refresh_tokens SET revoked_at = NOW() \
             WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id)
        .execute(&mut *connection)
        .await?;

        let deleted = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&mut *connection)
            .await?;
        Ok(deleted.rows_affected() == 1)
    }

    // 查询用户用户的角色列表
    pub async fn get_roles_by_user_id(&self, user_id: &Uuid) -> Result<Vec<UserRole>, UserError> {
        let roles = sqlx::query_scalar!(
            r#"
            SELECT role as "role: UserRole"
            FROM user_roles
            WHERE user_id = $1
            "#,
            user_id
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(roles)
    }

    /// admin 视图的单个用户（形状与列表逐字段一致，前端可直接替换列表里的那一行）。
    pub(crate) async fn admin_view_by_id(
        &self,
        id: &Uuid,
    ) -> Result<Option<UserListRecord>, UserError> {
        sqlx::query_as!(
            UserListRecord,
            r#"
            SELECT
                u.id,
                u.phone,
                u.email,
                u.display_name,
                u.avatar_url,
                u.status as "status: UserStatus",
                ARRAY(
                    SELECT ur.role
                    FROM user_roles ur
                    WHERE ur.user_id = u.id
                    ORDER BY CASE ur.role WHEN 'student' THEN 1 ELSE 2 END
                ) AS "roles!: Vec<UserRole>",
                EXISTS (
                    SELECT 1 FROM teacher_profiles tp WHERE tp.user_id = u.id AND tp.verified
                ) AS "teacher_verified!",
                u.created_at,
                u.updated_at
            FROM users u
            WHERE u.id = $1
            "#,
            id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(UserError::Db)
    }

    pub(crate) async fn user_list(
        &self,
        filter: &UserListFilter,
    ) -> Result<(Vec<UserListRecord>, i64), UserError> {
        // 创建一个事务
        let mut tx = self.pool.begin().await.map_err(UserError::Db)?;

        // 使用同一个只读事务
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(UserError::Db)?;

        // 查询总数
        let role = filter.role.as_ref().map(UserRole::as_str);
        let query_pattern = filter.query_pattern.as_deref();

        // 查询当前总数
        let total = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*)
            FROM users u
            WHERE (
                    $1::text IS NULL
                    OR EXISTS (
                        SELECT 1
                        FROM user_roles ur
                        WHERE ur.user_id = u.id AND ur.role = $1
                    )
                  )
              AND (
                    $2::text IS NULL
                    OR u.phone ILIKE $2 ESCAPE '\'
                    OR u.email ILIKE $2 ESCAPE '\'
                    OR u.display_name ILIKE $2 ESCAPE '\'
                  )
              AND ($3::timestamptz IS NULL OR u.created_at >= $3)
              AND ($4::timestamptz IS NULL OR u.created_at < $4)
            "#,
        )
        .bind(role)
        .bind(query_pattern)
        .bind(filter.registered_from)
        .bind(filter.registered_to)
        .fetch_one(&mut *tx)
        .await
        .map_err(UserError::Db)?;

        // 查询当前页
        let records = sqlx::query_as::<_, UserListRecord>(
            r#"
            SELECT
                a.id,
                a.phone,
                a.email,
                a.display_name,
                a.avatar_url,
                a.status,
                ARRAY(
                    SELECT ur.role
                    FROM user_roles ur
                    WHERE ur.user_id = a.id
                    ORDER BY CASE ur.role WHEN 'student' THEN 1 ELSE 2 END
                ) AS roles,
                EXISTS (
                    SELECT 1 FROM teacher_profiles tp WHERE tp.user_id = a.id AND tp.verified
                ) AS teacher_verified,
                a.created_at,
                a.updated_at
            FROM users a
            WHERE (
                    $1::text IS NULL
                    OR EXISTS (
                        SELECT 1
                        FROM user_roles ur
                        WHERE ur.user_id = a.id AND ur.role = $1
                    )
                  )
              AND (
                    $2::text IS NULL
                    OR a.phone ILIKE $2 ESCAPE '\'
                    OR a.email ILIKE $2 ESCAPE '\'
                    OR a.display_name ILIKE $2 ESCAPE '\'
                  )
              AND ($3::timestamptz IS NULL OR a.created_at >= $3)
              AND ($4::timestamptz IS NULL OR a.created_at < $4)
            ORDER BY a.created_at DESC, a.id DESC
            LIMIT $5 OFFSET $6
            "#,
        )
        .bind(role)
        .bind(query_pattern)
        .bind(filter.registered_from)
        .bind(filter.registered_to)
        .bind(filter.limit)
        .bind(filter.offset)
        .fetch_all(&mut *tx)
        .await
        .map_err(UserError::Db)?;

        tx.commit().await.map_err(UserError::Db)?;

        Ok((records, total))
    }
}

fn map_unique_violation(e: sqlx::Error) -> UserError {
    if is_unique_violation(&e, "users_phone_key") {
        return UserError::PhoneNumberAlreadyExists;
    }
    if is_unique_violation(&e, "users_email_key") {
        return UserError::EmailAlreadyExists;
    }

    UserError::Db(e)
}
