use std::sync::LazyLock;

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use regex::Regex;
use sha2::{Digest, Sha256};
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
    #[error("password is too weak")]
    TooWeak,
    #[error("password has appeared in a compromised-password list")]
    Compromised,
    #[error("failed to hash password")]
    HashFailed,
}

pub struct Password(String);

const PASSWORD_MIN_LEN: usize = 15;
const PASSWORD_MAX_LEN: usize = 128;
static COMPROMISED_PASSWORDS: LazyLock<std::collections::HashSet<&'static str>> =
    LazyLock::new(|| {
        include_str!("password-data/compromised-sha256.txt")
            .lines()
            .collect()
    });

impl Password {
    /// 仅在创建新密码时执行策略，登录必须原样验证，不能转换大小写或空格。
    pub fn parse(raw: &str) -> Result<Self, PasswordError> {
        if raw.is_empty() {
            return Err(PasswordError::Empty);
        }
        let length = raw.chars().count();
        if length < PASSWORD_MIN_LEN {
            return Err(PasswordError::TooShort);
        }
        if length > PASSWORD_MAX_LEN {
            return Err(PasswordError::TooLong);
        }
        let digest = Sha256::digest(raw.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if COMPROMISED_PASSWORDS.contains(digest.as_str()) {
            return Err(PasswordError::Compromised);
        }
        // 拒绝整串可预测重复，不按字符类别设组合门槛，也不拒绝长短语中的普通词。
        let lowercase = raw.to_lowercase();
        let chars: Vec<_> = lowercase.chars().collect();
        if (1..=4).any(|period| {
            chars
                .iter()
                .enumerate()
                .all(|(i, c)| *c == chars[i % period])
        }) || ["tshb", "tsz", "天生会背"].iter().any(|brand| {
            lowercase.strip_prefix(brand).is_some_and(|tail| {
                !tail.is_empty()
                    && tail
                        .chars()
                        .all(|c| c.is_ascii_digit() || c.is_ascii_punctuation())
            })
        }) {
            return Err(PasswordError::TooWeak);
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn parse_for_subjects(raw: &str, subjects: &[&str]) -> Result<Self, PasswordError> {
        let password = Self::parse(raw)?;
        let lowercase = raw.to_lowercase();
        if subjects.iter().any(|subject| {
            let subject = subject.to_lowercase();
            let local_part = subject.split_once('@').map(|(local, _)| local);
            [Some(subject.as_str()), local_part]
                .into_iter()
                .flatten()
                .any(|part| {
                    !part.is_empty()
                        && (lowercase == part
                            || (part.chars().count() >= 5 && lowercase.contains(part)))
                })
        }) {
            return Err(PasswordError::TooWeak);
        }
        Ok(password)
    }

    pub(crate) fn hash_raw(raw: &str) -> Result<String, PasswordError> {
        Argon2::default()
            .hash_password(raw.as_bytes())
            .map(|hash| hash.to_string())
            .map_err(|_| PasswordError::HashFailed)
    }

    pub async fn hash(self) -> Result<String, PasswordError> {
        spawn_blocking(move || Self::hash_raw(&self.0))
            .await
            .map_err(|_| PasswordError::HashFailed)?
    }

    /// bcrypt 只保留现有哈希的原样验证；所有新写入都使用 Argon2id。
    pub async fn verify_raw(raw: String, password_hash: String) -> bool {
        if raw.chars().count() > PASSWORD_MAX_LEN {
            return false;
        }
        spawn_blocking(move || {
            if password_hash.starts_with("$argon2id$") {
                PasswordHash::new(&password_hash).is_ok_and(|hash| {
                    Argon2::default()
                        .verify_password(raw.as_bytes(), &hash)
                        .is_ok()
                })
            } else if password_hash.starts_with("$2") {
                raw.len() <= 72 && bcrypt::verify(raw, &password_hash).unwrap_or(false)
            } else {
                false
            }
        })
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

/// 管理员临时密码与用户新密码使用同一策略。
pub fn validate_password(password: &str, subject: &str) -> Result<(), PasswordError> {
    Password::parse_for_subjects(password, &[subject]).map(|_| ())
}

#[cfg(test)]
mod password_tests {
    use super::*;

    #[test]
    fn unicode_length_boundaries_and_free_character_choices() {
        assert!(matches!(
            Password::parse("aB! 界🙂01234567"),
            Err(PasswordError::TooShort)
        ));
        assert!(Password::parse("aB! 界🙂012345678").is_ok());
        let long = format!("银河🙂{}", "界".repeat(125));
        assert_eq!(long.chars().count(), 128);
        assert!(Password::parse(&long).is_ok());
        assert!(matches!(
            Password::parse(&(long + "!")),
            Err(PasswordError::TooLong)
        ));
        assert!(Password::parse("orchard river silver cloud").is_ok());
        assert!(Password::parse("941807362590418735").is_ok());
    }

    #[test]
    fn weak_and_subject_checks_are_shared() {
        assert!(matches!(
            Password::parse("abcabcabcabcabcabc"),
            Err(PasswordError::TooWeak)
        ));
        assert!(matches!(
            Password::parse_for_subjects("My!13800138000Safe", &["13800138000"]),
            Err(PasswordError::TooWeak)
        ));
        assert!(
            Password::parse_for_subjects("welcome to the violet orchard!", &["user@example.com"])
                .is_ok()
        );
    }

    #[test]
    fn local_compromised_list_has_valid_hashes_and_is_checked() {
        assert!(!COMPROMISED_PASSWORDS.is_empty());
        assert!(
            COMPROMISED_PASSWORDS
                .iter()
                .all(|hash| hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()))
        );
        assert!(matches!(
            Password::parse("12345678901234567890"),
            Err(PasswordError::Compromised)
        ));
    }

    #[tokio::test]
    async fn argon2id_preserves_case_spaces_unicode_and_randomizes_salts() {
        let raw = " Mixed!密码🙂 river cloud ";
        let first = Password::parse(raw).unwrap().hash().await.unwrap();
        let second = Password::parse(raw).unwrap().hash().await.unwrap();
        assert!(first.starts_with("$argon2id$"));
        assert_ne!(first, second);
        assert!(Password::verify_raw(raw.to_owned(), first.clone()).await);
        assert!(!Password::verify_raw(raw.to_uppercase(), first.clone()).await);
        assert!(!Password::verify_raw(raw.trim().to_owned(), first).await);
        assert!(!Password::verify_raw(raw.to_owned(), "$argon2id$invalid".to_owned()).await);
        let long = format!("银河🙂{}", "界".repeat(125));
        let hash = Password::parse(&long).unwrap().hash().await.unwrap();
        assert!(Password::verify_raw(long, hash).await);
    }
}
