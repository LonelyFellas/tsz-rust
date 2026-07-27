use std::sync::LazyLock;

use bcrypt::{DEFAULT_COST, hash, verify};
use regex::Regex;
use tokio::task::spawn_blocking;

// 正则只编译一次（登录/注册是热路径，`Regex::new` 每次调用重编译代价不小）。
static PHONE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^1[3-9]\d{9}$").expect("phone 正则应可编译"));
static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$").expect("email 正则应可编译")
});

#[derive(Debug, thiserror::Error)]
pub enum PhoneError {
    #[error("phone is empty")]
    Empty,
    #[error("phone is invalid")]
    Invalid,
}

#[derive(Clone)]
pub struct Phone(String);

impl Phone {
    /// 解析并校验手机号：**先归一化（trim）再校验**，存入归一化后的值。
    /// 归一化必须先于校验——否则带首尾空格的合法号会被误判非法（回归教训）。
    pub fn parse(raw: &str) -> Result<Self, PhoneError> {
        let normalized = Self::normalize(raw);
        if normalized.is_empty() {
            return Err(PhoneError::Empty);
        }
        // `^1[3-9]\d{9}$` 已隐含「恰好 11 位」，无需另做长度检查。
        if !PHONE_RE.is_match(&normalized) {
            return Err(PhoneError::Invalid);
        }
        Ok(Phone(normalized))
    }

    /// 仅归一化（不校验）：trim。注册路径用它入库；`parse` 内部也先调它，
    /// 保证「入库形态」与「登录查询形态」一致（否则空格差异会导致查不到）。
    pub fn normalize(raw: &str) -> String {
        raw.trim().to_string()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
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

pub struct Password(String);

const PASSWORD_MIN_LEN: usize = 8;
const PASSWORD_MAX_LEN: usize = 72;

impl Password {
    /// 校验并封装一个**新**密码（注册 / 改密 / seed）——执行密码策略。
    /// 登录**不要**用它：登录只需比对已存哈希，见 [`Password::verify_raw`]。
    pub fn parse(raw: &str) -> Result<Self, PasswordError> {
        if raw.is_empty() {
            return Err(PasswordError::Empty);
        }
        // 长度按 Unicode 字符数而非字节：`len()` 是字节数，中文/emoji 会被高估。
        if raw.chars().count() < PASSWORD_MIN_LEN {
            return Err(PasswordError::TooShort);
        }
        // bcrypt 只取前 72 字节，超长直接拒绝而非静默截断。
        if raw.len() > PASSWORD_MAX_LEN {
            return Err(PasswordError::TooLong);
        }
        Ok(Password(raw.to_string()))
    }

    pub async fn hash(self) -> Result<String, PasswordError> {
        spawn_blocking(move || hash(&self.0, DEFAULT_COST))
            .await
            .expect("系统错误：密码哈希任务 join 失败")
            .map_err(|_| PasswordError::HashFailed)
    }

    /// 登录路径：把用户输入当**不透明串**比对已存哈希，**不执行密码策略**。
    /// 为什么不复用 `parse`：登录再跑注册策略会把「日后收紧策略」变成
    /// 「锁死存量用户」（旧密码不再满足新策略即登不进），且会让密码格式校验
    /// 插到锁定检查之前、破坏「锁定先于一切」的顺序。策略只属于创建密码那一刻。
    pub async fn verify_raw(raw: String, password_hash: String) -> bool {
        spawn_blocking(move || verify(&raw, &password_hash).unwrap_or(false))
            .await
            .unwrap_or(false)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    #[error("email is empty")]
    Empty,
    #[error("email is invalid")]
    Invalid,
}

pub struct Email(String);

impl Email {
    /// 解析并校验邮箱：**先归一化（trim + 小写）再校验**，存入归一化后的值。
    /// 小写化必须先于校验且随值入库——否则 `User@X.com` 与 `user@x.com` 会被当成
    /// 两个账号，且大小写不敏感登录会查不到（回归教训）。
    pub fn parse(raw: &str) -> Result<Self, EmailError> {
        let normalized = Self::normalize(raw);
        if normalized.is_empty() {
            return Err(EmailError::Empty);
        }
        if !EMAIL_RE.is_match(&normalized) {
            return Err(EmailError::Invalid);
        }
        Ok(Email(normalized))
    }

    /// 仅归一化（不校验）：trim + 小写。注册路径用它入库；`parse` 内部也先调它。
    pub fn normalize(raw: &str) -> String {
        raw.trim().to_lowercase()
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ValidatePasswordError {
    #[error("password must contain at least 12 characters")]
    TooShort,
    #[error("password must not be numeric only")]
    NumericOnly,
    #[error("password is too weak")]
    WeakPassword,
    #[error("password must not contain your subject")]
    ContainsSubject,
}
pub fn validate_password(password: &str, subject: &str) -> Result<(), ValidatePasswordError> {
    if password.chars().count() < 12 {
        return Err(ValidatePasswordError::TooShort);
    }

    if password.chars().all(|e| e.is_ascii_digit()) {
        return Err(ValidatePasswordError::NumericOnly);
    }

    let lowercase = password.to_lowercase();

    const WEAK_PATTERNS: &[&str] = &[
        "password", "qwerty", "asdfgh", "zxcvbn", "123456", "654321", "letmein", "iloveyou",
        "welcome", "admin123",
    ];

    if WEAK_PATTERNS
        .iter()
        .any(|pattern| lowercase.contains(pattern))
    {
        return Err(ValidatePasswordError::WeakPassword);
    }

    if !subject.is_empty() && password.contains(subject) {
        return Err(ValidatePasswordError::ContainsSubject);
    }

    Ok(())
}

#[cfg(test)]
mod admin_password_tests {
    use super::*;

    #[test]
    fn accepts_password_meeting_admin_and_common_policies() {
        let password = "Strong!Code42";

        assert!(validate_password(password, "13800138000").is_ok());
        assert!(Password::parse(password).is_ok());
    }

    #[test]
    fn admin_minimum_length_is_twelve_unicode_characters() {
        assert!(matches!(
            validate_password("Abcdef!2345", "13800138000"),
            Err(ValidatePasswordError::TooShort)
        ));

        // 管理员长度按字符而非 UTF-8 字节计算。
        let twelve_characters = format!("{}A", "界".repeat(11));
        assert_eq!(twelve_characters.chars().count(), 12);
        assert!(validate_password(&twelve_characters, "13800138000").is_ok());
    }

    #[test]
    fn rejects_numeric_only_password() {
        assert!(matches!(
            validate_password("987654321098", "13800138000"),
            Err(ValidatePasswordError::NumericOnly)
        ));
    }

    #[test]
    fn rejects_every_weak_pattern_case_insensitively() {
        for pattern in [
            "PASSWORD", "QWERTY", "ASDFGH", "ZXCVBN", "123456", "654321", "LETMEIN", "ILOVEYOU",
            "WELCOME", "ADMIN123",
        ] {
            let password = format!("Safe-{pattern}-X9!");
            assert!(
                matches!(
                    validate_password(&password, "13800138000"),
                    Err(ValidatePasswordError::WeakPassword)
                ),
                "weak pattern should be rejected: {pattern}"
            );
        }
    }

    #[test]
    fn rejects_password_containing_owners_phone() {
        assert!(matches!(
            validate_password("Safe!13800138000X", "13800138000"),
            Err(ValidatePasswordError::ContainsSubject)
        ));
    }

    #[test]
    fn empty_subject_does_not_reject_every_password() {
        assert!(validate_password("Strong!Code42", "").is_ok());
    }

    #[test]
    fn common_policy_still_enforces_bcrypt_byte_limit() {
        let password = format!("A!{}", "x".repeat(71));

        assert_eq!(password.len(), 73);
        assert!(validate_password(&password, "13800138000").is_ok());
        assert!(matches!(
            Password::parse(&password),
            Err(PasswordError::TooLong)
        ));
    }
}
