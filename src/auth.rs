use chrono::Duration;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode,
    errors::Error as JsonWebTokenError,
};
use thiserror::Error;

pub struct Claims {
    pub subject: Uuid,
    pub realm: Realm,
    pub role: String,
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
    sub: String,
    aud: String,
    role: String,
    iat: i64,
    exp: i64,
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
        let now = chrono::Utc::now();
        let payload = TokenPayload {
            sub: subject.to_string(),
            aud: self.realm.as_aud().to_string(),
            role: role.to_string(),
            iat: now.timestamp(),
            exp: now.timestamp() + self.ttl.num_seconds(),
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
            subject: Uuid::parse_str(&data.claims.sub).map_err(|_| TokenError::Invalid)?,
            realm: self.realm,
            role: data.claims.role,
        })
    }

    fn generate_validation(realm: &Realm) -> Validation {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[realm.as_aud()]);
        validation.set_required_spec_claims(&["exp", "sub", "aud"]);
        validation.leeway = 60;

        validation
    }
}
