use std::collections::HashMap;

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    lexicon::{
        dto::{
            Dialect, DialectVariantSlotV2, DraftMeaningsStepContent, EnglishTextV2, EntryKind,
            RichText, TextOrigin, WordDefinitionV2,
        },
        model::{
            CatalogPartRecord, CatalogSubPartRecord, ComponentTargetDraftRecord,
            DictionaryCandidateRecord, DictionaryContentRecord, DictionaryTermRecord,
            DraftSenseTargetRecord, EntryRecord, EntryReferenceRow,
            FormsSurfaceAcknowledgementRecord, IdempotencyRecord, InboundSenseReferenceRecord,
            ListEntryRecord, ListFilter, NewSentenceAssociation, NodeIdentityRecord,
            PublicationReadRecord, PublishedEntrySnapshotRecord, PublishedFormSurfaceRecord,
            RegionEvidenceRecord, RegionSurfaceRecord, RelatedSearchFilter, RelatedSearchRecord,
            ResolvedRelationTargetRecord, ResolvedSenseTargetRecord, SenseTargetKey,
            SentenceAssociationRecord, SentenceAssociationScanRecord, SentenceDiscoveryDraftRecord,
            SentenceDiscoverySurfaceRecord, StatsRecord, SurfaceInboundRelationRecord,
        },
        node_identity::{
            GRAMMAR_STRUCTURE_ROLE, RELATION_ROLE, SENSE_GROUP_ROLE, SENSE_ROLE, SENTENCE_ROLE,
            definition_role, text_variant_role,
        },
        normalization::{HEADWORD_NORMALIZATION_VERSION, sha256_json},
        sentence_association::association_form_source_kinds,
    },
    platform::is_foreign_key_violation,
};

mod dictionary;
mod entries;
mod lifecycle;
mod projections;
mod publications;
mod query;
mod sentence_associations;
mod sentence_target_discovery;
mod sentence_translations;
mod surface_writes;
mod surfaces;

use entries::*;
use projections::*;
pub(crate) use surface_writes::{SurfaceLockKey, SurfaceProjectionSource, surface_lock_keys};

#[derive(Debug, thiserror::Error)]
pub enum LexiconRepositoryError {
    #[error("lexicon invariant violated: {0}")]
    Invariant(&'static str),
    #[error("a referenced publication is being changed; retry the command")]
    TargetPublicationBusy,
    #[error("a referenced target changed while the entry was being written")]
    ReferenceTargetChanged,
    #[error("a surface match context is being changed; retry the command")]
    SurfaceContextBusy,
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
            SELECT request_hash, resource_id, response_status, response_body,
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
