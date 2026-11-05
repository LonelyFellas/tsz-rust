//! Shared HTTP error adaptation for coin operations, independent of admin workflows.

use super::model::CoinError;
use crate::error::{AppError, ErrorCode};

pub(crate) fn coin_error(error: CoinError) -> AppError {
    let code = match error {
        CoinError::Database(error) => {
            if error
                .as_database_error()
                .is_some_and(|e| e.constraint() == Some("coin_manual_event_once"))
            {
                return AppError::conflict(
                    ErrorCode::CoinSourceConflict,
                    None,
                    "event already recorded",
                );
            }
            return AppError::internal(error);
        }
        CoinError::InvalidAmount | CoinError::BalanceOverflow => ErrorCode::CoinInvalidAmount,
        CoinError::InvalidCommand => ErrorCode::InvalidRequestBody,
        CoinError::AccountUnavailable => ErrorCode::CoinAccountUnavailable,
        CoinError::WalletUnavailable | CoinError::InvalidTransition => {
            ErrorCode::CoinWalletUnavailable
        }
        CoinError::InsufficientBalance => ErrorCode::CoinInsufficientBalance,
        CoinError::IdempotencyConflict => ErrorCode::CoinIdempotencyConflict,
        CoinError::SourceConflict => ErrorCode::CoinSourceConflict,
        CoinError::BalanceChanged => ErrorCode::AccountDeletionBalanceChanged,
    };
    if matches!(
        code,
        ErrorCode::CoinInvalidAmount | ErrorCode::InvalidRequestBody
    ) {
        AppError::bad_request(code, "invalid coin request")
    } else {
        AppError::conflict(code, None, "coin operation rejected")
    }
}

#[cfg(test)]
mod tests {
    use super::{CoinError, coin_error};
    use axum::{body::to_bytes, response::IntoResponse};

    #[tokio::test]
    async fn boundary_coin_errors_preserve_status_code_and_detail() {
        use CoinError::*;
        for (error, status, code, detail) in [
            (
                InvalidAmount,
                400,
                "coin_invalid_amount",
                "invalid coin request",
            ),
            (
                BalanceOverflow,
                400,
                "coin_invalid_amount",
                "invalid coin request",
            ),
            (
                InvalidCommand,
                400,
                "invalid_request_body",
                "invalid coin request",
            ),
            (
                AccountUnavailable,
                409,
                "coin_account_unavailable",
                "coin operation rejected",
            ),
            (
                WalletUnavailable,
                409,
                "coin_wallet_unavailable",
                "coin operation rejected",
            ),
            (
                InvalidTransition,
                409,
                "coin_wallet_unavailable",
                "coin operation rejected",
            ),
            (
                InsufficientBalance,
                409,
                "coin_insufficient_balance",
                "coin operation rejected",
            ),
            (
                IdempotencyConflict,
                409,
                "coin_idempotency_conflict",
                "coin operation rejected",
            ),
            (
                SourceConflict,
                409,
                "coin_source_conflict",
                "coin operation rejected",
            ),
            (
                BalanceChanged,
                409,
                "account_deletion_balance_changed",
                "coin operation rejected",
            ),
            (
                Database(sqlx::Error::RowNotFound),
                500,
                "internal_error",
                "internal error",
            ),
            (
                Database(sqlx::Error::PoolTimedOut),
                503,
                "service_unavailable",
                "database temporarily unavailable; outcome may be unknown",
            ),
        ] {
            let response = coin_error(error).into_response();
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(
                response.headers()["content-type"],
                "application/problem+json"
            );
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["status"], status);
            assert_eq!(body["code"], code);
            assert_eq!(body["detail"], detail);
        }
    }
}
