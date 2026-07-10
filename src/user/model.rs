use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::user::repository::UserError;

#[derive(sqlx::Type, Debug, PartialEq)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum UserStatus {
    Active,
    Disabled,
}

#[derive(sqlx::Type, Debug, PartialEq, Clone, Copy)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum UserRole {
    Student,
    Teacher,
}

#[derive(sqlx::Type, Debug, PartialEq)]
#[sqlx(type_name = "text")]
pub enum CefrLevel {
    A1,
    A2,
    B1,
    B2,
    C1,
    C2,
}

#[derive(sqlx::Type, Debug, PartialEq)]
#[sqlx(type_name = "text")]
pub enum EnglishVariant {
    BrE,
    AmE,
}

#[derive(Debug, thiserror::Error)]
pub enum CodeError {
    #[error("code is empty")]
    Empty,
    #[error("code is invalid")]
    Invalid,
}

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("password is empty")]
    Empty,
    #[error("password is too short")]
    TooShort,
    #[error("password is too long")]
    TooLong,
    #[error("failed to hash password")]
    HashFailed,
}

#[derive(Debug, thiserror::Error)]
pub enum SubjectError {
    #[error("user already exists")]
    UserAlreadyExists,
    #[error("phone or email is missing")]
    PhoneOrEmailMissing,
    #[error("duplicate subject")]
    DuplicateSubject,
    #[error(transparent)]
    Repository(#[from] UserError),
}

#[derive(Debug, PartialEq)]
pub struct User {
    pub id: Uuid,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub status: UserStatus,
    pub last_active_role: Option<UserRole>,
    pub password_hash: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub avatar_url: String,
}
