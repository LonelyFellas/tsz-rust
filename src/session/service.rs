use chrono::{DateTime, Duration, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{
    auth::TokenError,
    platform::{generate_token_plaintext, hash_token},
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
pub struct RotatedRefresh {
    pub user_id: Uuid,
    pub refresh: IssuedRefresh, // 新枚明文 + 过期时间,handler 直接拿去做 cookie
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
        let (issued, row) = self.prepare_refresh(user_id);
        self.repository.insert(row).await?;
        Ok(issued)
    }

    pub async fn issue_in(
        &self,
        connection: &mut PgConnection,
        user_id: Uuid,
    ) -> Result<IssuedRefresh, SessionError> {
        let (issued, row) = self.prepare_refresh(user_id);
        RefreshTokenRepository::insert_in(connection, row).await?;
        Ok(issued)
    }

    fn prepare_refresh(&self, user_id: Uuid) -> (IssuedRefresh, NewRefreshToken) {
        let plaintext = generate_token_plaintext();
        let token_hash = hash_token(&plaintext);
        let expires_at = Utc::now() + self.refresh_ttl;
        (
            IssuedRefresh {
                plaintext,
                expires_at,
            },
            NewRefreshToken {
                id: Uuid::now_v7(),
                user_id,
                token_hash,
                expires_at,
            },
        )
    }

    ///  /auth/refresh 核心:哈希明文 → **同一事务**内 CAS 消费旧枚 + 落库新枚（`consume_and_insert`）
    /// → 返回属主 user_id + 新枚明文。不查账号状态（那是 handler 的活）。
    /// CAS 落空时验尸区分「无效」与「重放」：已轮换且未吊销 = 重放 → 该用户全端连坐吊销。
    pub async fn rotate(&self, plaintext: &str) -> Result<RotatedRefresh, SessionError> {
        let token_hash = hash_token(plaintext);
        let new_plaintext = generate_token_plaintext();
        let new_hash = hash_token(&new_plaintext);
        let expires_at = Utc::now() + self.refresh_ttl;
        if let Some(user_id) = self
            .repository
            .consume_and_insert(&token_hash, &new_hash, Uuid::now_v7(), expires_at)
            .await?
        {
            return Ok(RotatedRefresh {
                user_id,
                refresh: IssuedRefresh {
                    plaintext: new_plaintext,
                    expires_at,
                },
            });
        }

        // CAS 未抢到 -> 差一眼 这枚 token的尸体，区分「无效」与「重放」
        if let Some(token) = self.repository.find_by_hash(&token_hash).await?
            && let Some(rotated_at) = token.rotated_at
            && token.revoked_at.is_none()
        {
            // 加宽限期 20秒
            if Utc::now() - rotated_at < Duration::seconds(20) {
                // 窗口内：可能是丢包重试--不连坐，不发新枚，对外仍是同一个 报401
                return Err(SessionError::InvalidRefreshToken);
            }
            // 窗口外：已轮换且未吊销 = 重放 → 该用户全端连坐吊销。
            let n = self.repository.revoke_all_by_user_id(token.user_id).await?;
            tracing::warn!(
                user_id = %token.user_id, revoked = n,
                "refresh token replay detected; all sessions revoked"
            );
        }

        Err(SessionError::InvalidRefreshToken)
    }

    /// /auth/logout:哈希明文 → revoke_by_hash。幂等,永远 Ok(不泄露 token 是否存在)。
    pub async fn logout(&self, plaintext: &str) -> Result<(), SessionError> {
        let token_hash = hash_token(plaintext);
        self.repository.revoke_by_hash(&token_hash).await?;
        Ok(())
    }

    pub async fn peek_user_id(&self, plaintext: &str) -> Result<Option<Uuid>, SessionError> {
        let token_hash = hash_token(plaintext);
        Ok(self
            .repository
            .find_by_hash(&token_hash)
            .await?
            .map(|t| t.user_id))
    }
}

// crypto（generate_token_plaintext / hash_token）已上提到 platform，web 与 admin 两个
// 会话域共用一份；纯函数属性测试也随之集中在 src/platform/utils.rs。
