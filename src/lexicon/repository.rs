use std::collections::HashMap;

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    lexicon::{
        dto::{
            AdminWordV2, AdminWordV2Envelope, Dialect, DialectVariantSlotV2, EnglishTextV2,
            EntryKind, RichText, TextOrigin, WordDefinitionV2, WordHeadwordsV2,
        },
        model::{
            CatalogPartRecord, CatalogSubPartRecord, DictionaryCandidateRecord,
            DictionaryTermRecord, DuplicateRecord, EntryRecord, IdempotencyRecord,
            InboundSenseReferenceRecord, ListEntryRecord, ListFilter, NewPublicationSenseReference,
            NodeIdentityRecord, RegionSurfaceRecord, RelatedSearchRecord,
            ResolvedSenseTargetRecord, SenseTargetKey, StatsRecord,
        },
        node_identity::{
            BASE_FORM_ROLE, FORM_GROUP_ROLE, GRAMMAR_STRUCTURE_ROLE, POS_ROLE, PRONUNCIATION_ROLE,
            RELATION_ROLE, SENSE_GROUP_ROLE, SENSE_ROLE, SENTENCE_ROLE, definition_role,
            form_slot_role, form_variant_role, text_variant_role,
        },
        normalization::{HEADWORD_NORMALIZATION_VERSION, normalize_headword, sha256_json},
        provenance::headword_origin,
    },
    platform::is_unique_violation,
};

mod dictionary;
mod entries;
mod lifecycle;
mod projections;
mod publications;
mod query;

use entries::*;
use projections::*;

#[derive(Debug, thiserror::Error)]
pub enum LexiconRepositoryError {
    #[error("headword already exists")]
    DuplicateHeadword,
    #[error("lexicon invariant violated: {0}")]
    Invariant(&'static str),
    #[error("a referenced publication is being changed; retry the command")]
    TargetPublicationBusy,
    #[error("serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("database operation failed")]
    Database(#[source] sqlx::Error),
}

pub struct LexiconRepository {
    pool: PgPool,
}

impl LexiconRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub(crate) async fn idempotency(
        tx: &mut Transaction<'_, Postgres>,
        scope: &str,
        actor_id: Uuid,
        idempotency_key: Uuid,
    ) -> Result<Option<IdempotencyRecord>, LexiconRepositoryError> {
        let record = sqlx::query_as::<_, IdempotencyRecord>(
            r#"
            SELECT request_hash, resource_id, response_body,
                   expires_at <= now() AS expired
            FROM platform.idempotency_records
            WHERE scope = $1 AND actor_id = $2 AND idempotency_key = $3
            FOR UPDATE
            "#,
        )
        .bind(scope)
        .bind(actor_id)
        .bind(idempotency_key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        if record.as_ref().is_some_and(|record| record.expired) {
            sqlx::query(
                r#"
                DELETE FROM platform.idempotency_records
                WHERE scope = $1 AND actor_id = $2 AND idempotency_key = $3
                "#,
            )
            .bind(scope)
            .bind(actor_id)
            .bind(idempotency_key)
            .execute(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)?;
            return Ok(None);
        }

        Ok(record)
    }
}
