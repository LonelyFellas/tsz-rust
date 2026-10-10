//! Internal published-content boundary. Locks belong to the caller's transaction;
//! only new question generation needs to decode the current publication content.
use std::collections::HashMap;

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::dto::{AdminWordV3, DraftFormsStepContentV3, DraftMeaningsStepContentV3};
use crate::error::AppError;

pub(crate) use super::service::form_senses::allowed_form_senses;

/// Native V3 content without the administrative envelope. Never an HTTP DTO.
pub struct PublishedContentV3 {
    pub forms: DraftFormsStepContentV3,
    pub meanings: DraftMeaningsStepContentV3,
}

pub(crate) struct CurrentPublication {
    entry_id: Uuid,
    pub publication_id: Uuid,
    pub archive_generation: i64,
    snapshot: serde_json::Value,
}

impl CurrentPublication {
    /// Availability checks intentionally do not call this: an existing run uses
    /// its own prompt/answer snapshots, not the latest publication's content.
    pub fn content_v3(&self) -> Result<PublishedContentV3, AppError> {
        let word: AdminWordV3 =
            serde_json::from_value(self.snapshot.clone()).map_err(AppError::internal)?;
        if word.id != self.entry_id {
            return Err(AppError::internal(std::io::Error::other(
                "publication entry mismatch",
            )));
        }
        Ok(PublishedContentV3 {
            forms: word.forms,
            meanings: word.meanings,
        })
    }
}

/// Call after account/task/wordlist locks. Keep the sorted row locks until commit,
/// including unavailable entries, so publication and archive writers serialize.
pub(crate) async fn lock_entries_in(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<usize, AppError> {
    let rows: Vec<(Uuid, Option<Uuid>, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT id,current_publication_id,archived_at FROM lexicon.entries WHERE id=ANY($1) ORDER BY id FOR SHARE")
            .bind(ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    Ok(rows.len())
}

pub(crate) async fn read_current_in(
    tx: &mut Transaction<'_, Postgres>,
    entry_ids: &[Uuid],
) -> Result<HashMap<Uuid, CurrentPublication>, AppError> {
    lock_entries_in(tx, entry_ids).await?;
    let rows: Vec<(Uuid, Uuid, i64, serde_json::Value)> = sqlx::query_as("SELECT e.id,p.id,e.wordlist_archive_generation,p.snapshot FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE e.id=ANY($1) AND e.archived_at IS NULL AND p.content_schema_version=3")
        .bind(entry_ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    Ok(rows
        .into_iter()
        .map(|(entry_id, publication_id, archive_generation, snapshot)| {
            (
                entry_id,
                CurrentPublication {
                    entry_id,
                    publication_id,
                    archive_generation,
                    snapshot,
                },
            )
        })
        .collect())
}
