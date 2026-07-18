use sqlx::PgPool;
use uuid::Uuid;

use crate::user::model::{User, UserRole, UserStatus};

#[derive(Debug, thiserror::Error)]
pub enum UserError {
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
        let user = sqlx::query_as!(
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
        .ok_or(UserError::NotFound)?;

        Ok(user)
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
}

fn map_unique_violation(e: sqlx::Error) -> UserError {
    if let sqlx::Error::Database(db) = &e
        && db.code().as_deref() == Some("23505")
    {
        match db.constraint() {
            Some("users_phone_key") => return UserError::PhoneNumberAlreadyExists,
            Some("users_email_key") => return UserError::EmailAlreadyExists,
            _ => {}
        }
    }
    UserError::Db(e)
}
