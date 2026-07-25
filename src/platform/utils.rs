use std::sync::OnceLock;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bcrypt::{DEFAULT_COST, hash};
use sha2::{Digest, Sha256};

pub fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash("timing-balance", DEFAULT_COST).expect("系统错误：dummy hash 生成失败"))
}

/// 生成 refresh token 明文：32 字节系统级 CSPRNG → base64url（无 padding，43 字符）。
/// 用 getrandom（= rand 0.10 的 SysRng 底层）。
/// 两个会话域（web `session`、admin `session`）共用——纯机制、无表无 secret 无域知识，
/// 与 [`dummy_hash`] / [`Password`] 同属 platform 的加密原语层。
pub fn generate_token_plaintext() -> String {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).expect("系统级错误：OS 熵源不可用");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 哈希 token 明文：base64url(SHA-256(明文字节))。确定性、不加盐（设计文档 §4）——
/// 高熵串无需慢哈希/盐，且确定性才能按 token_hash 唯一索引 O(1) 查。
/// 改哈希策略要同步测试侧的镜像 `expected_hash`（tests/{session,admin_session}_*.rs）。
pub fn hash_token(plaintext: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(plaintext.as_bytes()))
}

#[cfg(test)]
mod tests {
    //! token 明文/哈希的纯函数规格测试（无 DB）。两个会话域的落库行为分别在
    //! tests/session_*.rs 与 tests/admin_session_*.rs（真库）里验。

    use super::{generate_token_plaintext, hash_token};

    /// 明文：32 字节 base64url 无 padding = 43 字符，且是 url-safe 字符集。
    #[test]
    fn plaintext_is_43_char_url_safe() {
        let s = generate_token_plaintext();
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
        let set: HashSet<String> = (0..1000).map(|_| generate_token_plaintext()).collect();
        assert_eq!(set.len(), 1000, "1000 次生成应全不相同");
    }

    /// 哈希：确定性、非明文、定长 43（sha256=32B → base64url 43 字符）。
    #[test]
    fn hash_is_deterministic_not_plaintext_and_fixed_len() {
        let p = generate_token_plaintext();
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
