//! Only application-owned events reach the service log. Never format error chains.
use std::error::Error;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                    (metadata.target() == "tsz_rust"
                        || metadata.target().starts_with("tsz_rust::")
                        || matches!(
                            metadata.target(),
                            "seed"
                                | "import_dictionary"
                                | "import_dictionary_content"
                                | "sync_speech_voices"
                                | "lexicon_v3_initial_headwords"
                                | "permission_migration_preview"
                                | "export_openapi"
                        ))
                        && *metadata.level() <= tracing::Level::INFO
                })),
        )
        .init();
    std::panic::set_hook(Box::new(|panic| {
        // Payloads and backtraces can contain credentials. Locations are compiled code.
        if let Some(location) = panic.location() {
            tracing::error!(
                event = "panic",
                file = location.file(),
                line = location.line()
            );
        } else {
            tracing::error!(event = "panic");
        }
    }));
}

pub fn error_kind(error: &(dyn Error + 'static)) -> &'static str {
    if let Some(error) = error.downcast_ref::<sqlx::Error>() {
        return match error {
            sqlx::Error::PoolTimedOut => "database_pool_timeout",
            sqlx::Error::Database(db) => match db.code().as_deref() {
                Some("57014") => "database_statement_timeout",
                Some("55P03") => "database_lock_timeout",
                Some("23505") => "database_unique_violation",
                Some("23503") => "database_foreign_key_violation",
                _ => "database",
            },
            _ => "database",
        };
    }
    if error.is::<std::io::Error>() {
        return "io";
    }
    if error.is::<deadpool_redis::redis::RedisError>() {
        return "redis";
    }
    if error.is::<crate::platform::storage::StorageError>() {
        return "storage";
    }
    // Inspect types only; arbitrary Display/Debug implementations are never invoked.
    error.source().map(error_kind).unwrap_or("unknown")
}

pub fn exit_on_error(stage: &'static str, result: anyhow::Result<()>) -> std::process::ExitCode {
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(
                event = "command_failed",
                stage,
                error_kind = error_kind(error.as_ref())
            );
            std::process::ExitCode::FAILURE
        }
    }
}

/// A detached maintenance loop must leave an observable failure if it stops.
pub fn spawn_worker(
    name: &'static str,
    future: impl std::future::Future<Output = ()> + Send + 'static,
) {
    use futures_util::FutureExt;
    tokio::spawn(async move {
        let panicked = std::panic::AssertUnwindSafe(future)
            .catch_unwind()
            .await
            .is_err();
        tracing::error!(event = "worker_stopped", worker = name, panicked);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    const SECRET: &str = "sensitive-password-cookie-otp-email-dsn-signed-url";

    #[test]
    fn reliability_log_child() {
        let Ok(mode) = std::env::var("TSZ_RELIABILITY_LOG_CHILD") else {
            return;
        };
        init();
        match mode.as_str() {
            "panic" => panic!("{SECRET}"),
            "http" => {
                use tower::ServiceExt;
                let runtime = tokio::runtime::Runtime::new().unwrap();
                runtime.block_on(async {
                    let pool = sqlx::postgres::PgPoolOptions::new()
                        .connect_lazy("postgres://localhost/unused")
                        .unwrap();
                    let app = crate::router(crate::state::AppState::for_test(pool));
                    let request = axum::http::Request::builder()
                        .method(SECRET)
                        .uri(format!("/{SECRET}?secret={SECRET}"))
                        .header("cookie", SECRET)
                        .header("x-request-id", SECRET)
                        .body(axum::body::Body::empty())
                        .unwrap();
                    let response = app.oneshot(request).await.unwrap();
                    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
                });
            }
            "worker" => {
                let runtime = tokio::runtime::Runtime::new().unwrap();
                runtime.block_on(async {
                    spawn_worker("synthetic_worker", async { panic!("{SECRET}") });
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                });
            }
            "error" => {
                use axum::response::IntoResponse;
                let _ = crate::error::AppError::internal(anyhow::anyhow!(SECRET)).into_response();
                tracing::error!(target: "sqlx::query", sql = SECRET);
                tracing::error!(target: "reqwest", response = SECRET);
                let status = exit_on_error("startup", Err(anyhow::anyhow!(SECRET)));
                assert_eq!(status, std::process::ExitCode::FAILURE);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn reliability_captures_real_stderr_without_error_or_panic_payloads() {
        for mode in ["panic", "error", "worker", "http"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "safe_log::tests::reliability_log_child",
                    "--nocapture",
                ])
                .env("TSZ_RELIABILITY_LOG_CHILD", mode)
                .env("RUST_LOG", "trace,sqlx=trace,reqwest=trace")
                .env("RUST_BACKTRACE", "full")
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(!stderr.contains(SECRET), "{stderr}");
            assert!(!stdout.contains(SECRET), "{stdout}");
            assert!(
                stderr.contains(if mode == "panic" {
                    "panic"
                } else if mode == "http" {
                    "http_response"
                } else if mode == "worker" {
                    "worker_stopped"
                } else {
                    "command_failed"
                }),
                "{stderr}"
            );
            assert_eq!(output.status.success(), mode != "panic");
        }
    }
}
