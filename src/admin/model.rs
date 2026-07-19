use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(sqlx::Type, Debug, PartialEq, Clone, Copy)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum AdminRole {
    SuperAdmin,
    Admin,
}

#[derive(sqlx::Type, Debug, PartialEq, Clone, Copy)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum AdminStatus {
    Active,
    Disabled,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Admin {
    pub id: Uuid,
    pub display_name: String,
    pub phone: String,
    pub password_hash: String,
    pub role: AdminRole,
    pub status: AdminStatus,
    pub must_change_password: bool,
    pub failed_login_count: i32,
    pub locked_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct NewAdmin {
    pub id: Uuid,
    pub display_name: String,
    pub phone: String,
    pub password_hash: String,
    pub role: AdminRole,
    pub must_change_password: bool,
}
