use bcrypt::{DEFAULT_COST, hash};
use chrono::{DateTime, Utc};
use unicode_general_category::{GeneralCategory, get_general_category};
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

impl UserRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            UserRole::Student => "student",
            UserRole::Teacher => "teacher",
        }
    }
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

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum DisplayNameError {
    #[error("display name cannot be empty")]
    Empty,
    #[error("display name cannot be longer than 50 characters")]
    TooLong,
    #[error("display name contains forbidden characters")]
    ForbiddenCharacters,
}

pub struct DisplayName(String);

impl DisplayName {
    pub fn parse(raw: &str) -> Result<Self, DisplayNameError> {
        // 1) trim
        let display_name = raw.trim();
        // 2) empty
        if display_name.is_empty() {
            return Err(DisplayNameError::Empty);
        }
        // 3) 长度按chars().count()数，大于50 -> TooLong (中文昵称是3不是9)
        let size = display_name.chars().count();

        if size > 50 {
            return Err(DisplayNameError::TooLong);
        }

        // 4) 禁用字符：< >
        if display_name.contains(['<', '>']) {
            return Err(DisplayNameError::ForbiddenCharacters);
        }
        // 5) 含 Unicode Cf（零宽 \u{200b}、BOM \u{feff}、bidi \u{202e}）→ Forbidden
        if display_name
            .chars()
            .any(|c| c.is_control() || get_general_category(c) == GeneralCategory::Format)
        {
            return Err(DisplayNameError::ForbiddenCharacters);
        }

        Ok(DisplayName(display_name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

// pub struct Code(String);

// impl Code {
//     pub fn parse(raw: &str) -> Result<Self, CodeError> {
//         // 1) trim
//         let code = raw.trim();
//         if code.is_empty() {
//             return Err(CodeError::Empty);
//         }
//         // 2) 必须6位数字
//         if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
//             return Err(CodeError::Invalid);
//         }
//         // 3) 验证码必须唯一, TODO: 实现
//         Ok(Code(code.to_string()))
//     }
//     pub fn into_string(self) -> String {
//         self.0
//     }
// }

pub struct Password(String);

impl Password {
    pub fn parse(raw: &str) -> Result<Self, PasswordError> {
        // 2) empty
        if raw.is_empty() {
            return Err(PasswordError::Empty);
        }
        // 3) 长度至少8位
        if raw.len() < 8 {
            return Err(PasswordError::TooShort);
        }
        // 4) 长度不超过72位
        if raw.len() > 72 {
            return Err(PasswordError::TooLong);
        }
        Ok(Password(raw.to_string()))
    }

    pub fn hash_password(&self) -> Result<String, PasswordError> {
        hash(&self.0, DEFAULT_COST).map_err(|_| PasswordError::HashFailed)
    }

    pub fn into_string(self) -> String {
        self.0
    }
}
