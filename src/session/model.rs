use chrono::{DateTime, Utc};
use uuid::Uuid;

/// refresh_token 表一行，query_as! 直接映射
#[derive(Debug)]
pub struct RefreshToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    // 被主动撤销，已不能用了
    pub revoked_at: Option<DateTime<Utc>>,
    // 被轮换消费，已换成新的 refresh token
    pub rotated_at: Option<DateTime<Utc>>,
    // 时间到了，自然死了
    pub expires_at: DateTime<Utc>,
    // 创建时间，和 DB 里的 timestamp 一致
    pub created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct NewRefreshToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
}
