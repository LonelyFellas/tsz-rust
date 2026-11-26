//! Bounded word-level autocomplete over the existing published surface indexes.
use super::{dto::*, service};
use crate::{auth::extract::AuthUser, error::AppError, lexicon::entry_search};
use sqlx::PgPool;

pub async fn match_import(
    pool: &PgPool,
    auth: &AuthUser,
    include_library: bool,
    input: WordlistImportMatchRequest,
) -> Result<WordlistImportMatches, AppError> {
    if input.words.is_empty() || input.words.len() > 100 {
        return Err(AppError::bad_request(
            crate::error::ErrorCode::InvalidQuery,
            "每批须包含 1–100 个词条",
        ));
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::lock_user(&mut tx, auth).await?;
    let rows = entry_search::exact_batch(&mut tx, &input.words).await?;
    let items = rows
        .into_iter()
        .map(|row| WordlistImportMatch {
            status: match row.len() {
                0 => WordlistImportMatchStatus::Unmatched,
                1 => WordlistImportMatchStatus::Matched,
                _ => WordlistImportMatchStatus::Ambiguous,
            },
            candidates: row
                .into_iter()
                .map(|row| candidate(row, include_library))
                .collect(),
        })
        .collect();
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(WordlistImportMatches { items })
}
pub async fn for_user(
    pool: &PgPool,
    auth: &AuthUser,
    include_library: bool,
    query: WordlistSuggestionQuery,
) -> Result<WordlistSuggestions, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    service::lock_user(&mut tx, auth).await?;
    let matches = entry_search::search_wordlist(&mut tx, &query.q).await?;
    let result = WordlistSuggestions {
        items: matches
            .into_iter()
            .map(|row| candidate(row, include_library))
            .collect(),
    };
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}

fn candidate(s: entry_search::EntrySuggestionSummary, include_library: bool) -> WordlistCandidate {
    WordlistCandidate {
        source_library: include_library.then_some(if s.is_next {
            WordlistSourceLibrary::LexiconNext
        } else {
            WordlistSourceLibrary::Legacy
        }),
        entry_id: s.entry_id,
        publication_id: s.publication_id,
        label: s.label,
        glosses: s.glosses,
    }
}
