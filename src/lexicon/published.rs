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
    pub excluded_form_ids: std::collections::HashSet<Uuid>,
    pub forms: DraftFormsStepContentV3,
    pub meanings: DraftMeaningsStepContentV3,
}

pub(crate) struct CurrentPublication {
    pub(crate) entry_id: Uuid,
    pub publication_id: Uuid,
    pub archive_generation: i64,
    snapshot: serde_json::Value,
    pub(crate) is_next: bool,
    pub(crate) teaching_eligible: bool,
}

impl CurrentPublication {
    /// Availability checks intentionally do not call this: an existing run uses
    /// its own prompt/answer snapshots, not the latest publication's content.
    pub(crate) fn known_equivalents(&self) -> impl Iterator<Item = &str> {
        self.snapshot
            .get("known_equivalents")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
    }
    pub fn content_v3(&self) -> Result<PublishedContentV3, AppError> {
        self.question_content(None)
    }
    pub(crate) fn question_content(
        &self,
        profile: Option<&str>,
    ) -> Result<PublishedContentV3, AppError> {
        if self.is_next {
            let doc: crate::lexicon_next::assessment_document::StudentDocument =
                serde_json::from_value(self.snapshot.clone()).map_err(AppError::internal)?;
            if doc.id != self.entry_id {
                return Err(AppError::internal(std::io::Error::other(
                    "reviewed publication entry mismatch",
                )));
            }
            return Ok(doc.for_questions(profile));
        }
        let mut snapshot = self.snapshot.clone();
        crate::lexicon::voice_versions::project_stored_dubbing(&mut snapshot);
        let word: AdminWordV3 = serde_json::from_value(snapshot).map_err(AppError::internal)?;
        if word.id != self.entry_id {
            return Err(AppError::internal(std::io::Error::other(
                "publication entry mismatch",
            )));
        }
        Ok(PublishedContentV3 {
            excluded_form_ids: Default::default(),
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
    let rows: Vec<(Uuid, Uuid, i64, serde_json::Value,bool)> = sqlx::query_as("SELECT e.id,p.id,e.wordlist_archive_generation,p.snapshot,e.teaching_eligible FROM lexicon.entries e JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id WHERE e.id=ANY($1) AND e.archived_at IS NULL AND p.content_schema_version=3")
        .bind(entry_ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    Ok(rows
        .into_iter()
        .map(
            |(entry_id, publication_id, archive_generation, snapshot, teaching_eligible)| {
                (
                    entry_id,
                    CurrentPublication {
                        entry_id,
                        publication_id,
                        archive_generation,
                        snapshot,
                        is_next: false,
                        teaching_eligible,
                    },
                )
            },
        )
        .collect())
}

/// Lock native rows in the same order as both source writers. A UNION view cannot take row locks.
pub(crate) async fn lock_learning_entries(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<usize, AppError> {
    let legacy = lock_entries_in(tx, ids).await?;
    let next = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM lexicon_next.entries WHERE id=ANY($1) ORDER BY id FOR SHARE",
    )
    .bind(ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    Ok(legacy + next.len())
}
pub(crate) async fn read_learning_in(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, CurrentPublication>, AppError> {
    lock_learning_entries(tx, ids).await?;
    let rows:Vec<(Uuid,Uuid,i64,serde_json::Value,bool,bool)>=sqlx::query_as(
        "SELECT entry_id,publication_id,wordlist_archive_generation,snapshot,is_next,teaching_eligible FROM learning_source.current_publications WHERE entry_id=ANY($1)"
    ).bind(ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    Ok(rows
        .into_iter()
        .map(
            |(
                entry_id,
                publication_id,
                archive_generation,
                snapshot,
                is_next,
                teaching_eligible,
            )| {
                (
                    entry_id,
                    CurrentPublication {
                        entry_id,
                        publication_id,
                        archive_generation,
                        snapshot,
                        is_next,
                        teaching_eligible,
                    },
                )
            },
        )
        .collect())
}
