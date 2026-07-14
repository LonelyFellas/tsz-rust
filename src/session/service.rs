use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    auth::TokenError,
    session::{
        model::NewRefreshToken,
        repository::{RefreshTokenError, RefreshTokenRepository},
    },
};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("invalid refresh token")]
    InvalidRefreshToken,
    #[error(transparent)]
    Repository(#[from] RefreshTokenError),
    #[error("signing error: {0}")]
    Signing(TokenError),
}

pub struct IssuedRefresh {
    pub plaintext: String,
    pub expires_at: DateTime<Utc>,
}

pub struct SessionService {
    repository: RefreshTokenRepository,
    refresh_ttl: Duration, // config 传入
}

impl SessionService {
    pub fn new(repository: RefreshTokenRepository, refresh_ttl: Duration) -> Self {
        Self {
            repository,
            refresh_ttl,
        }
    }

    /// login 调用：为user发一枚refresh。生成明文 -> 哈希 -> 存库 -> 返回明文
    pub async fn issue(&self, user_id: Uuid) -> Result<IssuedRefresh, SessionError> {
        let plaintext = generate_plaintext();
        let token_hash = hash_token(&plaintext);
        let expires_at = Utc::now() + self.refresh_ttl;

        self.repository
            .insert(NewRefreshToken {
                id: Uuid::now_v7(),
                user_id,
                token_hash,
                expires_at,
            })
            .await?;

        Ok(IssuedRefresh {
            plaintext,
            expires_at,
        })
    }
    /// /auth/refresh 步骤1:哈希明文 → CAS 消费旧枚 → 返回属主 user_id。不发新枚、不查账号状态。
    pub async fn rotate(&self, plaintext: &str) -> Result<Uuid, SessionError> {
        let token_hash = hash_token(plaintext);
        match self.repository.consume(&token_hash).await? {
            Some(user_id) => Ok(user_id),
            None => Err(SessionError::InvalidRefreshToken),
        }
    }

    /// /auth/logout:哈希明文 → revoke_by_hash。幂等,永远 Ok(不泄露 token 是否存在)。
    pub async fn logout(&self, plaintext: &str) -> Result<(), SessionError> {
        let token_hash = hash_token(plaintext);
        self.repository.revoke_by_hash(&token_hash).await?;
        Ok(())
    }
}

/// 生成明文
/// 32 字节系统级 CSPRNG → base64url（无 padding，43 字符）。
/// 用 getrandom（= rand 0.10 的 SysRng 底层）
fn generate_plaintext() -> String {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).expect("系统级错误：OS 熵源不可用");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 哈希明文
/// base64url(SHA-256(明文字节))。确定性、不加盐（设计文档 §4）——高熵串无需慢哈希/盐，
/// 且确定性才能按 token_hash 唯一索引 O(1) 查。
fn hash_token(plaintext: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(plaintext.as_bytes()))
}

#[cfg(test)]
mod tests {
    //! `generate_plaintext` / `hash_token` 的纯函数规格测试（无 DB）。
    //! 落库行为在 `tests/session_service.rs`（真库）。

    use super::{generate_plaintext, hash_token};

    /// 明文：32 字节 base64url 无 padding = 43 字符，且是 url-safe 字符集。
    #[test]
    fn plaintext_is_43_char_url_safe() {
        let s = generate_plaintext();
        assert_eq!(s.len(), 43, "32B base64url(no-pad) 应是 43 字符");
        assert!(
            s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "应只含 url-safe 字符（无 + / =）：{s}"
        );
    }

    /// 熵：大量生成基本互不相同（熵源坏了/退化成常量这条会挂）。
    #[test]
    fn plaintext_is_unique_across_calls() {
        use std::collections::HashSet;
        let set: HashSet<String> = (0..1000).map(|_| generate_plaintext()).collect();
        assert_eq!(set.len(), 1000, "1000 次生成应全不相同");
    }

    /// 哈希：确定性、非明文、定长 43（sha256=32B → base64url 43 字符）。
    #[test]
    fn hash_is_deterministic_not_plaintext_and_fixed_len() {
        let p = generate_plaintext();
        assert_eq!(hash_token(&p), hash_token(&p), "同一明文哈希应确定");
        assert_ne!(hash_token(&p), p, "哈希绝不应等于明文");
        assert_eq!(hash_token(&p).len(), 43, "sha256 的 base64url 应是 43 字符");
    }

    /// 不同明文得不同哈希（雪崩，抗碰撞的最起码性质）。
    #[test]
    fn different_input_different_hash() {
        assert_ne!(hash_token("aaa"), hash_token("bbb"));
    }
}
