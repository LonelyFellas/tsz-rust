pub mod dto;
pub mod handler;
pub mod service;
pub mod worker;

use crate::error::{AppError, ErrorCode};
use sqlx::PgConnection;
use uuid::Uuid;

/// Run as a separate statement AFTER obtaining the user's lock. clock_timestamp()
/// observes actual wall time, including time spent waiting for locks in this transaction.
pub async fn is_effective_in(conn: &mut PgConnection, user_id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM account_deletion_requests WHERE user_id=$1 AND status='pending' AND effective_at<=clock_timestamp())")
        .bind(user_id).fetch_one(conn).await
}
pub async fn ensure_not_effective_in(
    conn: &mut PgConnection,
    user_id: Uuid,
) -> Result<(), AppError> {
    if is_effective_in(conn, user_id)
        .await
        .map_err(AppError::internal)?
    {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
    Ok(())
}
