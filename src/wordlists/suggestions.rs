//! Bounded word-level autocomplete over the existing published surface indexes.
use super::{dto::*, review, service};
use crate::{admin::AdminAuth, auth::extract::AuthUser, error::AppError, lexicon::entry_search};
use sqlx::PgPool;
pub async fn for_user(
    pool: &PgPool,
    auth: &AuthUser,
    query: WordlistSuggestionQuery,
) -> Result<WordlistSuggestions, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::lock_user(&mut tx, auth).await?;
    let items = entry_search::search(&mut tx, &query.q, None).await?;
    let result = WordlistSuggestions {
        items: items
            .into_iter()
            .map(|item| WordlistCandidate {
                entry_id: item.entry_id,
                publication_id: item.publication_id,
                label: item.label,
                glosses: item.glosses,
            })
            .collect(),
    };
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn for_admin(
    pool: &PgPool,
    auth: &AdminAuth,
    query: WordlistSuggestionQuery,
) -> Result<WordlistSuggestions, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    review::lock_admin(&mut tx, auth, "wordlists.access").await?;
    let items = entry_search::search(&mut tx, &query.q, None).await?;
    let result = WordlistSuggestions {
        items: items
            .into_iter()
            .map(|item| WordlistCandidate {
                entry_id: item.entry_id,
                publication_id: item.publication_id,
                label: item.label,
                glosses: item.glosses,
            })
            .collect(),
    };
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
