use chrono::{DateTime, Utc};
use serde::Serialize;
use unicode_general_category::{GeneralCategory, get_general_category};
use uuid::Uuid;

use crate::user::repository::UserError;

#[derive(sqlx::Type, Debug, PartialEq, Clone, Copy)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum UserStatus {
    Active,
    Disabled,
}

#[derive(sqlx::Type, Debug, PartialEq, Clone, Copy, Serialize, utoipa::ToSchema)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
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

#[derive(Debug, PartialEq, Clone)]
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

impl User {
    /// 当前活跃角色，未设置时兜底 Student。
    /// JWT role claim 与各响应 `active_role`/`role` 的**唯一**策略点——
    /// 兜底规则要变只改这里，token 里的角色才不会与接口返回的角色漂移。
    pub fn active_role(&self) -> UserRole {
        self.last_active_role.unwrap_or(UserRole::Student)
    }
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

