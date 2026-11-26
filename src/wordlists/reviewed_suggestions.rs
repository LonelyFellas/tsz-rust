//! Recheck the matched review after locking the authoritative entry.
use super::service;
use crate::{
    error::AppError,
    lexicon::{entry_search::EntrySuggestionSummary, published},
};
use uuid::Uuid;

pub(super) async fn prefix(
    tx: &mut service::Tx<'_>,
    key: &str,
) -> Result<Vec<EntrySuggestionSummary>, AppError> {
    let found: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT p.entry_id,p.id FROM learning_source.next_surfaces n
         JOIN learning_source.reviewed_publications p ON p.id=n.publication_id
         JOIN learning_source.next_entries e ON e.id=p.entry_id AND e.current_publication_id=p.id
         WHERE n.surface=$1 OR (length($1)>=3 AND starts_with(n.surface,$1))
         GROUP BY p.entry_id,p.id ORDER BY bool_or(n.surface=$1) DESC,p.entry_id LIMIT 3",
    )
    .bind(key)
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    if found.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<_> = found.iter().map(|(id, _)| *id).collect();
    published::lock_learning_entries(tx, &ids).await?;
    let rows: Vec<EntrySuggestionSummary> =
        sqlx::query_as(include_str!("../lexicon/search_sql/suggest_summary.sql"))
            .bind(&ids)
            .fetch_all(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    Ok(rows
        .into_iter()
        .filter(|r| found.contains(&(r.entry_id, r.publication_id)))
        .collect())
}
