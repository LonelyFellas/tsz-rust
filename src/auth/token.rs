use chrono::Duration;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode,
    errors::Error as JsonWebTokenError,
};
use thiserror::Error;

#[derive(Clone)]
pub struct Claims {
    pub subject: Uuid,
    pub realm: Realm,
    pub role: String,
    pub security_version: i64,
}

#[derive(Copy, Clone)]
pub enum Realm {
    Web,
    Admin,
}

impl Realm {
    // 转换为 JWT 的 aud 字段
    fn as_aud(&self) -> &'static str {
        match self {
            Realm::Web => "web",
            Realm::Admin => "admin",
        }
    }
}

#[derive(Error, Debug)]
pub enum TokenError {
    #[error("token expired")]
    Expired,
    #[error("invalid token")]
    Invalid,
    #[error("signing error: {0}")]
    Signing(JsonWebTokenError),
}

pub struct TokenManager {
    encoding_key: EncodingKey, // new 时从 secret 预建
    decoding_key: DecodingKey, // 同上
    validation: Validation,    // 预建 algo=HS256 + aud=realm + require sub
    realm: Realm,
    ttl: Duration,
}

#[derive(Serialize, Deserialize)]
struct TokenPayload {
    sub: Uuid,
    aud: String,
    role: String,
    iat: i64,
    exp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    security_version: Option<i64>,
}

impl TokenManager {
    pub fn new(secret: &str, realm: Realm, ttl: Duration) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
            validation: Self::generate_validation(&realm),
            realm,
            ttl,
        }
    }

    /// 生成令牌
    pub fn generate(&self, subject: Uuid, role: &str) -> Result<String, TokenError> {
        self.generate_with_version(subject, role, 0)
    }

    pub fn generate_with_version(
        &self,
        subject: Uuid,
        role: &str,
        security_version: i64,
    ) -> Result<String, TokenError> {
        let now = chrono::Utc::now();
        let payload = TokenPayload {
            sub: subject,
            aud: self.realm.as_aud().to_string(),
            role: role.to_string(),
            iat: now.timestamp(),
            exp: now.timestamp() + self.ttl.num_seconds(),
            security_version: Some(security_version),
        };

        encode(&Header::new(Algorithm::HS256), &payload, &self.encoding_key)
            .map_err(TokenError::Signing)
    }
    /// 解析令牌
    pub fn parse(&self, token: &str) -> Result<Claims, TokenError> {
        let data = decode::<TokenPayload>(token, &self.decoding_key, &self.validation).map_err(
            |e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => TokenError::Expired,
                _ => TokenError::Invalid,
            },
        )?;
        Ok(Claims {
            subject: data.claims.sub,
            realm: self.realm,
            role: data.claims.role,
            security_version: data.claims.security_version.unwrap_or(0),
        })
    }

    fn generate_validation(realm: &Realm) -> Validation {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[realm.as_aud()]);
        validation.set_required_spec_claims(&["exp", "sub", "aud"]);
        validation.leeway = 60;

        validation
    }
    pub fn ttl_seconds(&self) -> i64 {
        self.ttl.num_seconds()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn both_realms_only_accept_hs256() {
        for realm in [Realm::Web, Realm::Admin] {
            let manager = TokenManager::new("test-secret", realm, Duration::minutes(5));
            assert_eq!(manager.validation.algorithms, vec![Algorithm::HS256]);
            let token = manager.generate(Uuid::now_v7(), "student").unwrap();
            assert_eq!(
                jsonwebtoken::decode_header(&token).unwrap().alg,
                Algorithm::HS256
            );
            assert!(manager.parse(&token).is_ok());
            let payload = TokenPayload {
                sub: Uuid::now_v7(),
                aud: realm.as_aud().to_owned(),
                role: "student".to_owned(),
                iat: chrono::Utc::now().timestamp(),
                exp: (chrono::Utc::now() + Duration::minutes(5)).timestamp(),
                security_version: None,
            };
            for algorithm in [Algorithm::HS384, Algorithm::HS512] {
                let token =
                    encode(&Header::new(algorithm), &payload, &manager.encoding_key).unwrap();
                assert!(matches!(manager.parse(&token), Err(TokenError::Invalid)));
            }
        }
    }

    #[test]
    fn subject_wire_format_and_legacy_tokens_remain_compatible() {
        let manager = TokenManager::new("test-secret", Realm::Web, Duration::minutes(5));
        let subject = Uuid::now_v7();
        let token = manager
            .generate_with_version(subject, "student", 7)
            .unwrap();
        let payload =
            decode::<serde_json::Value>(&token, &manager.decoding_key, &manager.validation)
                .unwrap()
                .claims;
        assert_eq!(payload["sub"], subject.to_string());
        assert_eq!(manager.parse(&token).unwrap().subject, subject);
        assert_eq!(manager.parse(&token).unwrap().security_version, 7);

        for raw in [subject.to_string(), subject.simple().to_string()] {
            let payload = json!({
                "sub": raw, "aud": "web", "role": "student",
                "iat": chrono::Utc::now().timestamp(),
                "exp": (chrono::Utc::now() + Duration::minutes(5)).timestamp()
            });
            let token = encode(
                &Header::new(Algorithm::HS256),
                &payload,
                &manager.encoding_key,
            )
            .unwrap();
            let claims = manager.parse(&token).unwrap();
            assert_eq!(claims.subject, subject);
            assert_eq!(claims.security_version, 0);

            let mut invalid = payload;
            invalid["sub"] = json!("invalid-uuid");
            let token = encode(
                &Header::new(Algorithm::HS256),
                &invalid,
                &manager.encoding_key,
            )
            .unwrap();
            assert!(matches!(manager.parse(&token), Err(TokenError::Invalid)));
        }
    }
}
