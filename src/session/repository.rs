use chrono::{DateTime, Utc};
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

    pub async fn consume_and_insert(
        &self,
        old_hash: &str,
        new_hash: &str,
        new_id: Uuid,
        expires_at: DateTime<Utc>,
    ) -> Result<Option<Uuid>, RefreshTokenError> {
        let mut tx = self.pool.begin().await?;
        let user_id = sqlx::query_scalar!(
            r#"UPDATE refresh_tokens SET rotated_at = NOW()
               WHERE token_hash = $1 AND rotated_at IS NULL AND revoked_at IS NULL AND expires_at > NOW()
               RETURNING user_id"#,
            old_hash
        )
        .fetch_optional(&mut *tx)   // ← sqlx 0.9 事务执行器写法:&mut *tx
        .await?;

        let Some(user_id) = user_id else {
            return Ok(None); // 提前 return,tx drop = 回滚
        };

        sqlx::query!(
            r#"INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at)
               VALUES ($1, $2, $3, $4)"#,
            new_id,
            user_id,
            new_hash,
            expires_at
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?; // 只有走到这里,旧枚的死亡才生效

        Ok(Some(user_id))
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
    pub async fn revoke_all_by_user_id(&self, user_id: Uuid) -> Result<u64, RefreshTokenError> {
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

    /// CAS 消费： 命中且未轮换/未吊销/未过期，才盖rotated_at, RETURNING user_id
    /// 抢到->OK(Some(user_id))；未命中（不存在/已轮换/已吊销/已过期）-> Ok(None)
    pub async fn consume(&self, token_hash: &str) -> Result<Option<Uuid>, RefreshTokenError> {
        let user_id= sqlx::query_scalar!(
            r#"
            UPDATE refresh_tokens SET rotated_at = NOW() WHERE token_hash = $1 AND rotated_at IS NULL AND revoked_at IS NULL AND expires_at > NOW()
            RETURNING user_id
            "#,
            token_hash
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(user_id)
    }

    /// logout: 按哈希吊销，幂等，返回影响行数
    pub async fn revoke_by_hash(&self, token_hash: &str) -> Result<u64, RefreshTokenError> {
        let row = sqlx::query!(
            r#"
            UPDATE refresh_tokens SET revoked_at = NOW() WHERE token_hash = $1 AND revoked_at IS NULL
            "#,
            token_hash
        )
        .execute(&self.pool)
        .await?;

        Ok(row.rows_affected())
    }
}
