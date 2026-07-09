use chrono::{DateTime, Utc};
use uuid::Uuid;

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
