use sqlx::PgPool;
use uuid::Uuid;

use crate::session::model::{NewRefreshToken, RefreshToken};

#[derive(Debug, thiserror::Error)]
pub enum RefreshTokenError {
    #[error("refresh token not found")]
    NotFound,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub struct RefreshTokenRepository {
    pool: PgPool,
}

impl RefreshTokenRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // issue 用: 插入一行。（login 只需要这个方法就能跑
    pub async fn insert(&self, row: NewRefreshToken) -> Result<RefreshToken, RefreshTokenError> {
        let row = sqlx::query_as!(
            RefreshToken,
            r#"
            INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at)
            VALUES ($1, $2, $3, $4)
            RETURNING id, user_id, token_hash, revoked_at, rotated_at, expires_at, created_at
            "#,
            row.id,
            row.user_id,
            row.token_hash,
            row.expires_at
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    // rotate/revoke 用： 用于哈希查行 （命中唯一索引 refresh_token_hash）
    pub async fn find_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<RefreshToken>, RefreshTokenError> {
        let row = sqlx::query_as!(
            RefreshToken,
            r#"
            SELECT id, user_id, token_hash, revoked_at, rotated_at, expires_at, created_at 
            FROM refresh_tokens WHERE token_hash = $1
            "#,
            token_hash
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// rotate 用： 标记旧行已轮换
    pub async fn mark_rotated(&self, id: Uuid) -> Result<(), RefreshTokenError> {
        sqlx::query!(
            r#"
            UPDATE refresh_tokens SET rotated_at = NOW() WHERE id = $1
            "#,
            id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// revoke 用： 吊销单行
    pub async fn revoke(&self, id: Uuid) -> Result<(), RefreshTokenError> {
        sqlx::query!(
            r#"
            UPDATE refresh_tokens SET revoked_at = NOW() WHERE id = $1
            "#,
            id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
    /// 改密 / 全设备登出用。返回吊销行数
    pub async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<u64, RefreshTokenError> {
        let row = sqlx::query!(
            r#"
            UPDATE refresh_tokens SET revoked_at = NOW() WHERE user_id = $1 AND revoked_at IS NULL
            "#,
            user_id
        )
        .execute(&self.pool)
        .await?;
        Ok(row.rows_affected())
    }
}
