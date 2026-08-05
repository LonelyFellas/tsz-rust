use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    platform::{EmailError, PhoneError, is_unique_violation},
    user::model::{User, UserListFilter, UserListRecord, UserRole, UserStatus},
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

#[derive(Debug, PartialEq)]
pub struct NewUser {
    pub id: Uuid,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub password_hash: String,
    pub display_name: String,
    pub first_role: UserRole,
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
        // 开事务
        let mut tx = self.pool.begin().await?;

        // 1) 插入users， RETURNING 拿回 DB 填的列
        let row = sqlx::query!(
            r#"
            INSERT INTO users (id, phone, email, password_hash, display_name, last_active_role)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING created_at, updated_at, status AS "status: UserStatus", avatar_url"#,
            input.id,
            input.phone,
            input.email,
            input.password_hash,
            input.display_name,
            input.first_role as UserRole
        )
        .fetch_one(&mut *tx)
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
        .execute(&mut *tx)
        .await?;

        let user = User {
            id: input.id,
            phone: input.phone,
            email: input.email,
            password_hash: input.password_hash,
            display_name: input.display_name,
            last_active_role: Some(input.first_role),
            status: UserStatus::Active,
            avatar_url: row.avatar_url,
            created_at: row.created_at,
            updated_at: row.updated_at,
        };

        tx.commit().await?;
        Ok(user)
    }

    /// 通过手机号/邮箱进行查询用户
    pub async fn get_by_identifier(&self, identifier: &str) -> Result<User, UserError> {
        sqlx::query_as!(
            User,
            r#"
            SELECT id, phone, email, password_hash, display_name, last_active_role as "last_active_role: UserRole", created_at, updated_at, status AS "status: UserStatus", avatar_url
            FROM users
            WHERE phone = $1 OR email = $1
            "#,
            identifier
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(UserError::NotFound)
    }
    /// 通过user_id 查询用户
    pub async fn get_by_id(&self, id: &Uuid) -> Result<User, UserError> {
        let user = sqlx::query_as!(
            User,
            r#"
                SELECT id, phone, email, password_hash, display_name, last_active_role as "last_active_role: UserRole", created_at, updated_at, status AS "status: UserStatus", avatar_url
                FROM users
                WHERE id = $1
            "#,
            id
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(UserError::NotFound)?;

        Ok(user)
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
        let phone_pattern = filter.phone_pattern.as_deref();
        let email_pattern = filter.email_pattern.as_deref();
        let display_name_pattern = filter.display_name_pattern.as_deref();

        // 查询当前总数
        let total = sqlx::query_scalar::<_, i64>(
            r#"
            select count(*)
            from users a
            where ($1::text is null or a.role = $1)
              and ($2::text is null or a.phone ilike $2 escape '\')
              and ($3::text is null or a.email ilike $3 escape '\')
              and ($4::text is null or a.display_name ilike $4 escape '\')
            "#,
        )
        .bind(role)
        .bind(phone_pattern)
        .bind(email_pattern)
        .bind(display_name_pattern)
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
                a.status,
                s.cefr_level AS cefr_level,
                s.english_variant AS english_variant,
                a.avatar_url,
                a.created_at,
                a.updated_at
            FROM users a
            LEFT JOIN status_profiles s
                on a.id = s.user_id
            WHERE ($1::text IS NULL OR a.role = $1)
              AND ($2::text IS NULL OR a.phone ILIKE $2 ESCAPE '\')
              AND ($3::text IS NULL OR a.email ILIKE $3 ESCAPE '\')
              AND ($4::text IS NULL OR a.display_name ILIKE $4 ESCAPE '\')
            ORDER BY a.created_at DESC, a.id DESC
            LIMIT $5 OFFSET $6
            "#,
        )
        .bind(role)
        .bind(phone_pattern)
        .bind(email_pattern)
        .bind(display_name_pattern)
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
