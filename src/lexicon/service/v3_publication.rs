use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::*;
use crate::lexicon::dto::{
    ActivatePublicationV3Input, AdminWordAny, AdminWordAnyEnvelope, AdminWordStatus, AdminWordV2,
    AdminWordV3, DraftFormsStepContentV3, DraftMeaningsStepContent, DraftMeaningsStepContentV3,
    PersistedWordStep, PhraseComponentUsageV3, PublishAdminWordV3Input,
    SentenceAssociationsStateV2, StepSaveIntent, WordFormTypeV3, WordRegionalVariantsV3,
};
use crate::lexicon::model::{
    NewPublicationSenseReference, PublicationSenseReferenceKind, PublicationTargetContentScope,
};

const V3_PUBLISH_SCOPE: &str = "lexicon.entry.publish.v3";
const V3_ACTIVATE_SCOPE: &str = "lexicon.publication.activate.v3";

#[derive(Debug, Clone, sqlx::FromRow)]
struct V3PublicationState {
    origin: String,
    migration_batch_id: Option<Uuid>,
    source_publication_id: Option<Uuid>,
    publication_canary_enabled: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct VersionedPublication {
    id: Uuid,
    entry_id: Uuid,
    publication_number: i32,
    source_revision: i64,
    content_schema_version: i16,
    snapshot: Value,
    published_at: DateTime<Utc>,
}

impl LexiconService {
    pub async fn publish_v3(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        idempotency_key: Uuid,
        input: PublishAdminWordV3Input,
        allow_automatic_associations: bool,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        let request_hash = sha256_json(&serde_json::json!({
            "entry_id": entry_id,
            "input": input,
        }))
        .map_err(serialization_error)?;
        let mut word = self.get_v3(entry_id).await?;
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        lock_v3_idempotency(&mut tx, V3_PUBLISH_SCOPE, actor_id, idempotency_key).await?;
        if let Some(existing) =
            LexiconRepository::idempotency(&mut tx, V3_PUBLISH_SCOPE, actor_id, idempotency_key)
                .await
                .map_err(repository_error)?
        {
            if existing.request_hash != request_hash {
                return Err(LexiconServiceError::IdempotencyConflict);
            }
            tx.commit().await.map_err(database_error)?;
            return serde_json::from_value(existing.response_body).map_err(serialization_error);
        }
        lock_v3_migration_entry(&mut tx, entry_id).await?;
        preflight_v3_publication_eligibility(&mut tx, entry_id).await?;

        let verified_surface = self
            .confirm_v3_publish_surface(
                &mut tx,
                actor_id,
                entry_id,
                input.base_revision,
                &word.forms,
                input.confirmed_surface_match_token.as_deref(),
            )
            .await?;

        let record = LexiconRepository::entry_by_id_for_update(&mut tx, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        ensure_locked_v3_entry(&record, input.base_revision)?;
        if word.revision != record.revision {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        let state = v3_publication_state_for_update(&mut tx, entry_id).await?;
        ensure_v3_publication_eligibility(&mut tx, entry_id, &state).await?;

        let mut issues =
            crate::lexicon::v3_contract::validate_forms(&word.forms, StepSaveIntent::Complete);
        issues.extend(crate::lexicon::v3_contract::validate_meanings(
            &word.meanings,
        ));
        issues.extend(
            crate::lexicon::v3_contract::validate_complete_definition_grammar(&word.meanings),
        );
        issues.extend(crate::lexicon::v3_contract::validate_aggregate_node_limit(
            &word.forms,
            &word.meanings,
        ));
        let validation_forms = v3_meaning_validation_forms(&word.forms);
        let catalog = self
            .catalog_context_for_reference(&mut tx, &validation_forms)
            .await?;
        let mut relational_meanings = v3_meanings_to_v2(&word.meanings)?;
        crate::lexicon::sentence_association::clear_sentence_associations(&mut relational_meanings);
        let rich_text_is_safe = canonicalize_meanings(&mut relational_meanings);
        let semantic_issues = validate_meanings(
            entry_id,
            &validation_forms,
            &relational_meanings,
            &v3_meaning_validation_headwords(),
            &catalog.sub_part_parents,
        );
        if !rich_text_is_safe
            || !meaning_storage_is_safe(
                entry_id,
                &validation_forms,
                &relational_meanings,
                &catalog.sub_part_parents,
            )
        {
            issues.extend(meanings_storage_issues(entry_id, semantic_issues));
        } else {
            issues.extend(semantic_issues);
        }
        if !issues.is_empty() {
            return Err(v3_validation_failed(issues));
        }

        let (materialization_issues, newly_bound) = self
            .resolve_pending_relation_targets(
                &mut tx,
                actor_id,
                request_id,
                entry_id,
                &mut relational_meanings,
                PendingRelationResolution::Materialize,
            )
            .await?;
        if !materialization_issues.is_empty() {
            return Err(v3_validation_failed(materialization_issues));
        }
        let reference_resolution = resolve_meaning_references(
            &mut tx,
            entry_id,
            &mut relational_meanings,
            ReferenceResolutionMode::Verify,
            true,
            &newly_bound,
        )
        .await?;
        if !reference_resolution.issues.is_empty() {
            return Err(v3_validation_failed(reference_resolution.issues));
        }
        let mut publication_references = reference_resolution.publication_references;
        publication_references
            .extend(phrase_component_publication_references(&mut tx, &word.forms).await?);
        if !newly_bound.is_empty() {
            let canonical_v3_meanings = v2_meanings_to_v3(relational_meanings.clone())?;
            let editor_meanings =
                serde_json::to_value(&canonical_v3_meanings).map_err(serialization_error)?;
            LexiconRepository::sync_canonical_meanings(
                &mut tx,
                entry_id,
                &relational_meanings,
                &editor_meanings,
                &catalog.sub_part_ids,
            )
            .await
            .map_err(repository_error)?;
            word.meanings = canonical_v3_meanings;
        }
        ensure_no_removed_inbound_senses(&mut tx, entry_id, &relational_meanings).await?;

        if let Some(publication) =
            v3_publication_by_revision_for_update(&mut tx, entry_id, word.revision).await?
        {
            let previous_publication_id = record.current_publication_id;
            word.status = AdminWordStatus::Published;
            word.published_revision = Some(publication.source_revision);
            word.has_unpublished_changes = false;
            word.published_at = Some(publication.published_at);
            if previous_publication_id != Some(publication.id) {
                let next_lifecycle_revision = record.lifecycle_revision + 1;
                replace_current_publication_surfaces_v3(
                    &mut tx,
                    publication.id,
                    publication.source_revision,
                    word.kind,
                    &word.forms,
                )
                .await?;
                update_current_publication_pointer(
                    &mut tx,
                    entry_id,
                    publication.id,
                    actor_id,
                    record.revision,
                    record.lifecycle_revision,
                    next_lifecycle_revision,
                )
                .await?;
                word.lifecycle_revision = next_lifecycle_revision;
                word.updated_at = Utc::now();
                insert_v3_activation_event(
                    &mut tx,
                    entry_id,
                    previous_publication_id,
                    &publication,
                    word.lifecycle_revision,
                )
                .await?;
            }
            let response = v3_envelope(word);
            if let Some(confirmation) = verified_surface.as_ref() {
                LexiconRepository::insert_command_surface_confirmation_audits(
                    &mut tx,
                    actor_id,
                    request_id,
                    entry_id,
                    record.revision,
                    confirmation,
                )
                .await
                .map_err(repository_error)?;
            }
            insert_v3_command_response(
                &mut tx,
                V3_PUBLISH_SCOPE,
                actor_id,
                request_id,
                idempotency_key,
                &request_hash,
                entry_id,
                Some(publication.id),
                201,
                "lexicon.entry.publish.v3",
                serde_json::json!({
                    "publication_id": publication.id,
                    "publication_number": publication.publication_number,
                    "reused": true,
                }),
                &response,
            )
            .await?;
            tx.commit().await.map_err(database_error)?;
            remove_verified_surface_confirmation(self, verified_surface).await;
            return Ok(response);
        }

        Self::refresh_sentence_associations(
            &mut tx,
            entry_id,
            &relational_meanings,
            true,
            allow_automatic_associations,
            None,
        )
        .await?;
        word.meanings = v2_meanings_to_v3(relational_meanings)?;
        Self::hydrate_v3_sentence_associations_in(&mut tx, entry_id, &mut word.meanings).await?;
        word.status = AdminWordStatus::Published;
        word.published_revision = Some(word.revision);
        word.has_unpublished_changes = false;
        word.published_at = Some(Utc::now());
        word.lifecycle_revision = record.lifecycle_revision + 1;
        word.updated_at = Utc::now();

        let publication =
            insert_v3_publication(&mut tx, actor_id, &word, &publication_references).await?;
        replace_current_publication_surfaces_v3(
            &mut tx,
            publication.id,
            publication.source_revision,
            word.kind,
            &word.forms,
        )
        .await?;
        update_current_publication_pointer(
            &mut tx,
            entry_id,
            publication.id,
            actor_id,
            record.revision,
            record.lifecycle_revision,
            word.lifecycle_revision,
        )
        .await?;
        insert_v3_publish_event(&mut tx, entry_id, &publication, state.source_publication_id)
            .await?;
        let response = v3_envelope(word);
        if let Some(confirmation) = verified_surface.as_ref() {
            LexiconRepository::insert_command_surface_confirmation_audits(
                &mut tx,
                actor_id,
                request_id,
                entry_id,
                record.revision,
                confirmation,
            )
            .await
            .map_err(repository_error)?;
        }
        insert_v3_command_response(
            &mut tx,
            V3_PUBLISH_SCOPE,
            actor_id,
            request_id,
            idempotency_key,
            &request_hash,
            entry_id,
            Some(publication.id),
            201,
            "lexicon.entry.publish.v3",
            serde_json::json!({
                "publication_id": publication.id,
                "publication_number": publication.publication_number,
                "source_v2_publication_id": state.source_publication_id,
            }),
            &response,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        remove_verified_surface_confirmation(self, verified_surface).await;
        Ok(response)
    }

    pub async fn activate_publication_v3(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        publication_id: Uuid,
        idempotency_key: Uuid,
        input: ActivatePublicationV3Input,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        let request_hash = sha256_json(&serde_json::json!({
            "entry_id": entry_id,
            "publication_id": publication_id,
            "input": input,
        }))
        .map_err(serialization_error)?;
        let mut word = self.get_v3(entry_id).await?;
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        lock_v3_idempotency(&mut tx, V3_ACTIVATE_SCOPE, actor_id, idempotency_key).await?;
        if let Some(existing) =
            LexiconRepository::idempotency(&mut tx, V3_ACTIVATE_SCOPE, actor_id, idempotency_key)
                .await
                .map_err(repository_error)?
        {
            if existing.request_hash != request_hash {
                return Err(LexiconServiceError::IdempotencyConflict);
            }
            tx.commit().await.map_err(database_error)?;
            return serde_json::from_value(existing.response_body).map_err(serialization_error);
        }
        lock_v3_migration_entry(&mut tx, entry_id).await?;
        preflight_v3_publication_eligibility(&mut tx, entry_id).await?;

        let publication = versioned_publication(&mut tx, entry_id, publication_id)
            .await?
            .ok_or(LexiconServiceError::PublicationNotFound)?;
        ensure_publication_snapshot_identity(&publication, entry_id)?;
        if let Some(forms) = publication_forms_for_activation(&publication)? {
            let issues = crate::lexicon::v3_contract::validate_dialect_rules(&forms);
            if !issues.is_empty() {
                return Err(v3_validation_failed(issues));
            }
        }
        let verified_surface = self
            .confirm_v3_activation_surface(
                &mut tx,
                actor_id,
                entry_id,
                publication.id,
                input.base_revision,
                input.base_lifecycle_revision,
                &publication.snapshot,
                input.confirmed_surface_match_token.as_deref(),
            )
            .await?;

        let record = LexiconRepository::entry_by_id_for_update(&mut tx, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        ensure_locked_v3_entry(&record, input.base_revision)?;
        if record.lifecycle_revision != input.base_lifecycle_revision {
            return Err(LexiconServiceError::LifecycleRevisionConflict {
                current_lifecycle_revision: record.lifecycle_revision,
            });
        }
        if word.revision != record.revision {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        let state = v3_publication_state_for_update(&mut tx, entry_id).await?;
        ensure_v3_publication_eligibility(&mut tx, entry_id, &state).await?;

        if record.current_publication_id == Some(publication.id) {
            word.status = AdminWordStatus::Published;
            word.published_revision = Some(publication.source_revision);
            word.has_unpublished_changes = word.revision != publication.source_revision;
            word.published_at = Some(publication.published_at);
            let response = v3_envelope(word);
            if let Some(confirmation) = verified_surface.as_ref() {
                LexiconRepository::insert_command_surface_confirmation_audits(
                    &mut tx,
                    actor_id,
                    request_id,
                    entry_id,
                    record.revision,
                    confirmation,
                )
                .await
                .map_err(repository_error)?;
            }
            insert_v3_command_response(
                &mut tx,
                V3_ACTIVATE_SCOPE,
                actor_id,
                request_id,
                idempotency_key,
                &request_hash,
                entry_id,
                Some(publication.id),
                200,
                "lexicon.publication.activate.v3",
                serde_json::json!({
                    "publication_id": publication.id,
                    "no_op": true,
                }),
                &response,
            )
            .await?;
            tx.commit().await.map_err(database_error)?;
            remove_verified_surface_confirmation(self, verified_surface).await;
            return Ok(response);
        }

        LexiconRepository::lock_outbound_sense_ref_targets_for_publication(&mut tx, publication.id)
            .await
            .map_err(repository_error)?;
        let unavailable = LexiconRepository::unavailable_outbound_sense_refs_for_publication(
            &mut tx,
            publication.id,
        )
        .await
        .map_err(repository_error)?;
        if !unavailable.is_empty() {
            return Err(LexiconServiceError::EntryHasUnavailablePublicationRefs(
                unavailable,
            ));
        }
        let publication_meanings = publication_meanings_for_reference_validation(&publication)?;
        ensure_no_removed_inbound_senses(&mut tx, entry_id, &publication_meanings).await?;

        replace_current_publication_surfaces_from_snapshot(&mut tx, &publication).await?;
        let next_lifecycle_revision = record.lifecycle_revision + 1;
        update_current_publication_pointer(
            &mut tx,
            entry_id,
            publication.id,
            actor_id,
            record.revision,
            record.lifecycle_revision,
            next_lifecycle_revision,
        )
        .await?;
        word.status = AdminWordStatus::Published;
        word.published_revision = Some(publication.source_revision);
        word.has_unpublished_changes = word.revision != publication.source_revision;
        word.published_at = Some(publication.published_at);
        word.lifecycle_revision = next_lifecycle_revision;
        word.updated_at = Utc::now();
        insert_v3_activation_event(
            &mut tx,
            entry_id,
            record.current_publication_id,
            &publication,
            next_lifecycle_revision,
        )
        .await?;
        let response = v3_envelope(word);
        if let Some(confirmation) = verified_surface.as_ref() {
            LexiconRepository::insert_command_surface_confirmation_audits(
                &mut tx,
                actor_id,
                request_id,
                entry_id,
                record.revision,
                confirmation,
            )
            .await
            .map_err(repository_error)?;
        }
        insert_v3_command_response(
            &mut tx,
            V3_ACTIVATE_SCOPE,
            actor_id,
            request_id,
            idempotency_key,
            &request_hash,
            entry_id,
            Some(publication.id),
            200,
            "lexicon.publication.activate.v3",
            serde_json::json!({
                "publication_id": publication.id,
                "previous_publication_id": record.current_publication_id,
                "content_schema_version": publication.content_schema_version,
            }),
            &response,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        remove_verified_surface_confirmation(self, verified_surface).await;
        Ok(response)
    }
}

async fn remove_verified_surface_confirmation(
    service: &LexiconService,
    confirmation: Option<VerifiedSurfaceConfirmation>,
) {
    if let Some(confirmation) = confirmation
        && let Err(error) = service
            .surface_snapshots
            .remove_verified(&confirmation)
            .await
    {
        tracing::warn!(
            %error,
            snapshot_id = %confirmation.snapshot_id,
            "completed V3 publication command but failed to remove surface confirmation"
        );
    }
}

fn v3_envelope(word: AdminWordV3) -> AdminWordAnyEnvelope {
    AdminWordAnyEnvelope {
        word: AdminWordAny::V3(Box::new(word)),
    }
}

async fn lock_v3_idempotency(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    actor_id: Uuid,
    idempotency_key: Uuid,
) -> Result<(), LexiconServiceError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("{scope}:{actor_id}:{idempotency_key}"))
        .execute(&mut **tx)
        .await
        .map(|_| ())
        .map_err(database_error)
}

fn ensure_locked_v3_entry(
    record: &EntryRecord,
    base_revision: i64,
) -> Result<(), LexiconServiceError> {
    if record.content_schema_version != 3 {
        return Err(LexiconServiceError::UnsupportedSchemaVersion(
            record.content_schema_version,
        ));
    }
    if record.archived_at.is_some() {
        return Err(LexiconServiceError::EntryArchived);
    }
    if record.revision != base_revision {
        return Err(LexiconServiceError::RevisionConflict {
            current_revision: record.revision,
        });
    }
    Ok(())
}

async fn v3_publication_state_for_update(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<V3PublicationState, LexiconServiceError> {
    sqlx::query_as::<_, V3PublicationState>(
        r#"
        SELECT origin, migration_batch_id, source_publication_id,
               publication_canary_enabled
        FROM lexicon.v3_entry_state
        WHERE entry_id = $1
        FOR UPDATE
        "#,
    )
    .bind(entry_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .ok_or_else(invariant_record)
}

async fn preflight_v3_publication_eligibility(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<(), LexiconServiceError> {
    let state = sqlx::query_as::<_, V3PublicationState>(
        r#"
        SELECT origin, migration_batch_id, source_publication_id,
               publication_canary_enabled
        FROM lexicon.v3_entry_state
        WHERE entry_id = $1
        "#,
    )
    .bind(entry_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .ok_or(LexiconServiceError::V3PublicationRequiresMigrationCanary)?;
    match state.origin.as_str() {
        "native" => ensure_native_publication_state(&state),
        "migrated_v2" => preflight_migration_canary(tx, entry_id).await,
        _ => Err(invariant_record()),
    }
}

async fn preflight_migration_canary(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<(), LexiconServiceError> {
    let eligible: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM lexicon.v3_entry_state state
            JOIN lexicon.v3_migration_batches batch
              ON batch.id = state.migration_batch_id
            JOIN lexicon.v3_migration_entries migration
              ON migration.batch_id = batch.id
             AND migration.entry_id = state.entry_id
            WHERE state.entry_id = $1
              AND state.origin = 'migrated_v2'
              AND state.publication_canary_enabled = TRUE
              AND state.source_publication_id IS NOT NULL
              AND migration.source_current_publication_id = state.source_publication_id
              AND batch.status = 'verified'
              AND migration.status = 'verified'
        )
        "#,
    )
    .bind(entry_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    if eligible {
        Ok(())
    } else {
        Err(LexiconServiceError::V3PublicationRequiresMigrationCanary)
    }
}

async fn lock_v3_migration_entry(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<(), LexiconServiceError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("lexicon.v3-migration.entry:{entry_id}"))
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    Ok(())
}

async fn ensure_migration_canary(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    state: &V3PublicationState,
) -> Result<(), LexiconServiceError> {
    let (Some(batch_id), Some(source_publication_id)) =
        (state.migration_batch_id, state.source_publication_id)
    else {
        return Err(LexiconServiceError::V3PublicationRequiresMigrationCanary);
    };
    if state.origin != "migrated_v2" || !state.publication_canary_enabled {
        return Err(LexiconServiceError::V3PublicationRequiresMigrationCanary);
    }
    let verified = sqlx::query_as::<_, (String, String, Option<Uuid>)>(
        r#"
        SELECT batch.status, migration.status,
               migration.source_current_publication_id
        FROM lexicon.v3_migration_batches batch
        JOIN lexicon.v3_migration_entries migration
          ON migration.batch_id = batch.id
        WHERE batch.id = $1 AND migration.entry_id = $2
        FOR SHARE OF batch, migration
        "#,
    )
    .bind(batch_id)
    .bind(entry_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    if verified.is_some_and(
        |(batch_status, entry_status, migration_source_publication_id)| {
            batch_status == "verified"
                && entry_status == "verified"
                && migration_source_publication_id == Some(source_publication_id)
        },
    ) {
        Ok(())
    } else {
        Err(LexiconServiceError::V3PublicationRequiresMigrationCanary)
    }
}

async fn ensure_v3_publication_eligibility(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    state: &V3PublicationState,
) -> Result<(), LexiconServiceError> {
    match state.origin.as_str() {
        "native" => ensure_native_publication_state(state),
        "migrated_v2" => ensure_migration_canary(tx, entry_id, state).await,
        _ => Err(invariant_record()),
    }
}

fn ensure_native_publication_state(state: &V3PublicationState) -> Result<(), LexiconServiceError> {
    if state.migration_batch_id.is_none()
        && state.source_publication_id.is_none()
        && !state.publication_canary_enabled
    {
        Ok(())
    } else {
        Err(invariant_record())
    }
}

async fn v3_publication_by_revision_for_update(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    source_revision: i64,
) -> Result<Option<VersionedPublication>, LexiconServiceError> {
    sqlx::query_as::<_, VersionedPublication>(
        r#"
        SELECT id, entry_id, publication_number, source_revision,
               content_schema_version, snapshot, published_at
        FROM lexicon.entry_publications
        WHERE entry_id = $1
          AND content_schema_version = 3
          AND source_revision = $2
        FOR UPDATE
        "#,
    )
    .bind(entry_id)
    .bind(source_revision)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)
}

async fn versioned_publication(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    publication_id: Uuid,
) -> Result<Option<VersionedPublication>, LexiconServiceError> {
    sqlx::query_as::<_, VersionedPublication>(
        r#"
        SELECT id, entry_id, publication_number, source_revision,
               content_schema_version, snapshot, published_at
        FROM lexicon.entry_publications
        WHERE entry_id = $1 AND id = $2
        "#,
    )
    .bind(entry_id)
    .bind(publication_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)
}

fn v3_meanings_to_v2(
    meanings: &DraftMeaningsStepContentV3,
) -> Result<DraftMeaningsStepContent, LexiconServiceError> {
    let mut meanings = meanings.clone();
    crate::lexicon::v3_contract::normalize_sentence_translations(&mut meanings);
    clear_v3_sentence_associations(&mut meanings);
    serde_json::from_value(serde_json::to_value(meanings).map_err(serialization_error)?)
        .map_err(serialization_error)
}

fn v2_meanings_to_v3(
    meanings: DraftMeaningsStepContent,
) -> Result<DraftMeaningsStepContentV3, LexiconServiceError> {
    let mut meanings =
        serde_json::from_value(serde_json::to_value(meanings).map_err(serialization_error)?)
            .map_err(serialization_error)?;
    crate::lexicon::v3_contract::normalize_sentence_translations(&mut meanings);
    Ok(meanings)
}

fn publication_meanings_for_reference_validation(
    publication: &VersionedPublication,
) -> Result<DraftMeaningsStepContent, LexiconServiceError> {
    match publication.content_schema_version {
        2 => serde_json::from_value::<AdminWordV2>(publication.snapshot.clone())
            .map(|word| word.meanings)
            .map_err(serialization_error),
        3 => serde_json::from_value::<AdminWordV3>(publication.snapshot.clone())
            .map_err(serialization_error)
            .and_then(|word| v3_meanings_to_v2(&word.meanings)),
        version => Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
    }
}

fn publication_forms_for_activation(
    publication: &VersionedPublication,
) -> Result<Option<DraftFormsStepContentV3>, LexiconServiceError> {
    match publication.content_schema_version {
        2 => Ok(None),
        3 => serde_json::from_value::<AdminWordV3>(publication.snapshot.clone())
            .map(|word| Some(word.forms))
            .map_err(serialization_error),
        version => Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
    }
}

async fn phrase_component_publication_references(
    tx: &mut Transaction<'_, Postgres>,
    forms: &DraftFormsStepContentV3,
) -> Result<Vec<NewPublicationSenseReference>, LexiconServiceError> {
    let components = forms
        .pos
        .iter()
        .flat_map(|pos| &pos.forms)
        .flat_map(|form| match &form.regional_variants {
            WordRegionalVariantsV3::Common { common } => common.component_usages.clone(),
            WordRegionalVariantsV3::UkUs { uk, us } => uk
                .component_usages
                .iter()
                .chain(&us.component_usages)
                .cloned()
                .collect(),
        })
        .collect::<Vec<_>>();
    let mut requested = components
        .iter()
        .filter_map(|component| match component {
            PhraseComponentUsageV3::Resolved {
                target_word_id,
                target_publication_id,
                target_sense_id,
                ..
            } => Some((*target_word_id, *target_publication_id, *target_sense_id)),
            PhraseComponentUsageV3::Unresolved { .. } => None,
        })
        .collect::<Vec<_>>();
    requested.sort_unstable();
    requested.dedup();
    let target_entry_ids = requested
        .iter()
        .map(|(entry_id, _, _)| *entry_id)
        .collect::<Vec<_>>();
    let target_publication_ids = requested
        .iter()
        .map(|(_, publication_id, _)| *publication_id)
        .collect::<Vec<_>>();
    let target_sense_ids = requested
        .iter()
        .map(|(_, _, sense_id)| *sense_id)
        .collect::<Vec<_>>();
    let target_revisions = LexiconRepository::phrase_component_publication_targets_for_publish(
        tx,
        &target_entry_ids,
        &target_publication_ids,
        &target_sense_ids,
    )
    .await
    .map_err(repository_error)?
    .into_iter()
    .map(|(entry_id, publication_id, sense_id, revision)| {
        ((entry_id, publication_id, sense_id), revision)
    })
    .collect::<HashMap<_, _>>();
    if target_revisions.len() != requested.len() {
        return Err(LexiconServiceError::ReferenceConflict);
    }
    let mut references = Vec::new();
    for component in components {
        let PhraseComponentUsageV3::Resolved {
            id,
            target_word_id,
            target_publication_id,
            target_sense_id,
            ..
        } = component
        else {
            continue;
        };
        let target_revision = *target_revisions
            .get(&(target_word_id, target_publication_id, target_sense_id))
            .ok_or(LexiconServiceError::ReferenceConflict)?;
        references.push(NewPublicationSenseReference {
            source_node_id: id,
            reference_kind: PublicationSenseReferenceKind::PhraseComponent,
            target_entry_id: target_word_id,
            target_sense_id,
            target_publication_id: Some(target_publication_id),
            target_content_scope: PublicationTargetContentScope::Publication,
            target_revision,
        });
    }
    Ok(references)
}

async fn ensure_no_removed_inbound_senses(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    meanings: &DraftMeaningsStepContent,
) -> Result<(), LexiconServiceError> {
    let retained_sense_ids = meanings
        .pos
        .iter()
        .flat_map(|pos| pos.senses.iter().map(|sense| sense.id))
        .collect::<Vec<_>>();
    let inbound = LexiconRepository::current_inbound_sense_refs(tx, entry_id, &retained_sense_ids)
        .await
        .map_err(repository_error)?;
    if inbound.is_empty() {
        return Ok(());
    }
    Err(v3_validation_failed(
        inbound
            .into_iter()
            .map(|reference| DraftValidationIssue {
                step: PersistedWordStep::Meanings,
                node_id: reference.target_sense_id,
                field: "senses".to_owned(),
                code: "sense_has_inbound_publication_refs".to_owned(),
                message: "该词义仍被其他词条的当前发布版本引用".to_owned(),
                reference_location: Some(DraftReferenceLocation {
                    source_entry_id: reference.source_entry_id,
                    source_publication_id: reference.source_publication_id,
                    source_node_id: reference.source_node_id,
                    reference_kind: reference.reference_kind,
                }),
                node_location: None,
            })
            .collect(),
    ))
}

async fn insert_v3_publication(
    tx: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
    word: &AdminWordV3,
    sense_references: &[NewPublicationSenseReference],
) -> Result<VersionedPublication, LexiconServiceError> {
    let publication_id = Uuid::now_v7();
    let publication_number = sqlx::query_scalar::<_, i32>(
        "SELECT COALESCE(MAX(publication_number), 0) + 1 FROM lexicon.entry_publications WHERE entry_id = $1",
    )
    .bind(word.id)
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    let mut snapshot_word = word.clone();
    clear_v3_sentence_associations(&mut snapshot_word.meanings);
    let snapshot = serde_json::to_value(&snapshot_word).map_err(serialization_error)?;
    let snapshot_hash = sha256_json(&snapshot_word).map_err(serialization_error)?;
    let published_at = word.published_at.ok_or_else(invariant_record)?;
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publications (
            id, entry_id, publication_number, source_revision,
            content_schema_version, snapshot, snapshot_hash,
            published_by_admin_id, published_at
        ) VALUES ($1, $2, $3, $4, 3, $5, $6, $7, $8)
        "#,
    )
    .bind(publication_id)
    .bind(word.id)
    .bind(publication_number)
    .bind(word.revision)
    .bind(&snapshot)
    .bind(snapshot_hash)
    .bind(actor_id)
    .bind(published_at)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;

    insert_v3_publication_nodes(tx, publication_id, word).await?;
    insert_publication_catalog_refs(tx, publication_id, word.id).await?;
    insert_publication_sense_refs(tx, publication_id, word.id, sense_references).await?;
    sqlx::query(
        "UPDATE lexicon.nodes SET first_published_at = COALESCE(first_published_at, $2) WHERE entry_id = $1 AND removed_from_draft_at IS NULL",
    )
    .bind(word.id)
    .bind(published_at)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(VersionedPublication {
        id: publication_id,
        entry_id: word.id,
        publication_number,
        source_revision: word.revision,
        content_schema_version: 3,
        snapshot,
        published_at,
    })
}

async fn insert_v3_publication_nodes(
    tx: &mut Transaction<'_, Postgres>,
    publication_id: Uuid,
    word: &AdminWordV3,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_nodes (
            publication_id, entry_id, node_id, node_type, content_hash
        )
        SELECT $1, node.entry_id, node.id, node.node_type, variant.content_hash
        FROM lexicon.nodes node
        LEFT JOIN lexicon.text_variants variant
          ON variant.id = node.id AND variant.entry_id = node.entry_id
        WHERE node.entry_id = $2 AND node.removed_from_draft_at IS NULL
        "#,
    )
    .bind(publication_id)
    .bind(word.id)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    for pos in &word.forms.pos {
        for form in &pos.forms {
            match &form.regional_variants {
                crate::lexicon::dto::WordRegionalVariantsV3::Common { common } => {
                    update_v3_pronunciation_hashes(
                        tx,
                        publication_id,
                        &common.spelling,
                        "common",
                        &common.pronunciations,
                    )
                    .await?;
                }
                crate::lexicon::dto::WordRegionalVariantsV3::UkUs { uk, us } => {
                    update_v3_pronunciation_hashes(
                        tx,
                        publication_id,
                        &uk.spelling,
                        "uk",
                        &uk.pronunciations,
                    )
                    .await?;
                    update_v3_pronunciation_hashes(
                        tx,
                        publication_id,
                        &us.spelling,
                        "us",
                        &us.pronunciations,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}

async fn update_v3_pronunciation_hashes(
    tx: &mut Transaction<'_, Postgres>,
    publication_id: Uuid,
    spelling: &str,
    dialect: &str,
    pronunciations: &[crate::lexicon::dto::WordPronunciationV3],
) -> Result<(), LexiconServiceError> {
    for pronunciation in pronunciations {
        let hash = sha256_json(&serde_json::json!({
            "spelling": spelling,
            "dialect": dialect,
            "dict_phonetic": pronunciation.dict_phonetic,
            "actual_pron": pronunciation.actual_pron,
            "style": pronunciation.style,
        }))
        .map_err(serialization_error)?;
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.entry_publication_nodes
            SET content_hash = $3
            WHERE publication_id = $1
              AND node_id = $2
              AND node_type = 'pronunciation'
            "#,
        )
        .bind(publication_id)
        .bind(pronunciation.id)
        .bind(hash)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
        if updated.rows_affected() != 1 {
            return Err(invariant_record());
        }
    }
    Ok(())
}

async fn insert_publication_catalog_refs(
    tx: &mut Transaction<'_, Postgres>,
    publication_id: Uuid,
    entry_id: Uuid,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_part_of_speech_refs (
            publication_id, entry_id, source_node_id, part_of_speech_id
        )
        SELECT $1, entry_id, id, part_of_speech_id
        FROM lexicon.entry_pos
        WHERE entry_id = $2
        "#,
    )
    .bind(publication_id)
    .bind(entry_id)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_sub_part_of_speech_refs (
            publication_id, entry_id, source_node_id, sub_part_of_speech_id
        )
        SELECT $1, entry_id, id, sub_part_of_speech_id
        FROM lexicon.senses
        WHERE entry_id = $2 AND sub_part_of_speech_id IS NOT NULL
        "#,
    )
    .bind(publication_id)
    .bind(entry_id)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

async fn insert_publication_sense_refs(
    tx: &mut Transaction<'_, Postgres>,
    publication_id: Uuid,
    entry_id: Uuid,
    references: &[NewPublicationSenseReference],
) -> Result<(), LexiconServiceError> {
    for reference in references {
        if reference.target_entry_id == entry_id {
            return Err(invariant_record());
        }
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_publication_sense_refs (
                publication_id, entry_id, source_node_id, reference_kind,
                target_entry_id, target_sense_id, target_publication_id,
                target_content_scope, target_revision
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(publication_id)
        .bind(entry_id)
        .bind(reference.source_node_id)
        .bind(reference.reference_kind.as_str())
        .bind(reference.target_entry_id)
        .bind(reference.target_sense_id)
        .bind(reference.target_publication_id)
        .bind(reference.target_content_scope.as_str())
        .bind(reference.target_revision)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    Ok(())
}

fn clear_v3_sentence_associations(meanings: &mut DraftMeaningsStepContentV3) {
    for pos in &mut meanings.pos {
        for sense in &mut pos.senses {
            for sentence in &mut sense.sentences {
                sentence.associations.clear();
                sentence.associations_state = SentenceAssociationsStateV2::Unresolved;
            }
        }
    }
}

async fn update_current_publication_pointer(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    publication_id: Uuid,
    actor_id: Uuid,
    revision: i64,
    expected_lifecycle_revision: i64,
    next_lifecycle_revision: i64,
) -> Result<(), LexiconServiceError> {
    let updated = sqlx::query(
        r#"
        UPDATE lexicon.entries
        SET current_publication_id = $2,
            draft_based_on_publication_id = $2,
            updated_by_admin_id = $3,
            updated_at = now(),
            lifecycle_revision = $6
        WHERE id = $1
          AND revision = $4
          AND lifecycle_revision = $5
          AND content_schema_version = 3
        "#,
    )
    .bind(entry_id)
    .bind(publication_id)
    .bind(actor_id)
    .bind(revision)
    .bind(expected_lifecycle_revision)
    .bind(next_lifecycle_revision)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() == 1 {
        Ok(())
    } else {
        Err(LexiconServiceError::LifecycleRevisionConflict {
            current_lifecycle_revision: expected_lifecycle_revision,
        })
    }
}

async fn replace_current_publication_surfaces_v3(
    tx: &mut Transaction<'_, Postgres>,
    publication_id: Uuid,
    source_revision: i64,
    entry_kind: WordEntryKindV3,
    forms: &DraftFormsStepContentV3,
) -> Result<(), LexiconServiceError> {
    let entry_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT entry_id FROM lexicon.entry_publications WHERE id = $1",
    )
    .bind(publication_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    let sources = crate::lexicon::v3_projection::form_variant_sources(entry_id, forms)
        .map_err(|_| invariant_record())?;
    let event_offset = retire_current_publication_surfaces(tx, entry_id, source_revision).await?;
    for source in sources {
        upsert_v3_publication_surface(
            tx,
            &source,
            entry_kind,
            publication_id,
            source_revision,
            event_offset,
        )
        .await?;
    }
    insert_surface_projection_event(
        tx,
        entry_id,
        publication_id,
        source_revision,
        event_offset,
        3,
    )
    .await
}

async fn replace_current_publication_surfaces_from_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    publication: &VersionedPublication,
) -> Result<(), LexiconServiceError> {
    match publication.content_schema_version {
        2 => {
            let word: AdminWordV2 = serde_json::from_value(publication.snapshot.clone())
                .map_err(serialization_error)?;
            let sources = crate::lexicon::repository::surface_projection_sources(&word)
                .map_err(|_| invariant_record())?;
            let event_offset = retire_current_publication_surfaces(
                tx,
                publication.entry_id,
                publication.source_revision,
            )
            .await?;
            for source in sources {
                upsert_v2_publication_surface(
                    tx,
                    &source,
                    publication.id,
                    publication.source_revision,
                    event_offset,
                )
                .await?;
            }
            insert_surface_projection_event(
                tx,
                publication.entry_id,
                publication.id,
                publication.source_revision,
                event_offset,
                publication.content_schema_version,
            )
            .await
        }
        3 => {
            let word: AdminWordV3 = serde_json::from_value(publication.snapshot.clone())
                .map_err(serialization_error)?;
            replace_current_publication_surfaces_v3(
                tx,
                publication.id,
                publication.source_revision,
                word.kind,
                &word.forms,
            )
            .await
        }
        version => Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
    }
}

async fn retire_current_publication_surfaces(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    source_revision: i64,
) -> Result<i64, LexiconServiceError> {
    let event_offset = sqlx::query_scalar::<_, i64>(
        "SELECT nextval('lexicon.surface_projection_event_offset_seq')",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"
        UPDATE lexicon.surface_sources
        SET source_revision = $2,
            event_offset = $3,
            is_deleted = TRUE,
            updated_at = now()
        WHERE entry_id = $1 AND content_scope = 'current_publication'
        "#,
    )
    .bind(entry_id)
    .bind(source_revision)
    .bind(event_offset)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(event_offset)
}

async fn upsert_v3_publication_surface(
    tx: &mut Transaction<'_, Postgres>,
    source: &crate::lexicon::v3_projection::V3FormVariantSurfaceSource,
    entry_kind: WordEntryKindV3,
    publication_id: Uuid,
    source_revision: i64,
    event_offset: i64,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO lexicon.surface_sources (
            entry_id, source_id, source_kind, source_node_id,
            language, entry_kind, dialect, dialect_scope,
            surface, normalized_surface, normalization_version,
            source_revision, event_offset, is_deleted, content_scope,
            publication_id, pos_id, pos, form_type, content_schema_version,
            form_id, variant_id, group_ids, projection_version, updated_at
        ) VALUES (
            $1, $2, 'form_variant', $3,
            'en', $4, $5, $6,
            $7, $8, $9,
            $10, $11, FALSE, 'current_publication',
            $12, $13, $14, $15, 3,
            $16, $17, $18, $19, now()
        )
        ON CONFLICT (source_id, content_scope, dialect_scope, normalization_version)
        DO UPDATE SET
            entry_id = EXCLUDED.entry_id,
            source_kind = EXCLUDED.source_kind,
            source_node_id = EXCLUDED.source_node_id,
            dialect = EXCLUDED.dialect,
            surface = EXCLUDED.surface,
            normalized_surface = EXCLUDED.normalized_surface,
            source_revision = EXCLUDED.source_revision,
            event_offset = EXCLUDED.event_offset,
            is_deleted = FALSE,
            publication_id = EXCLUDED.publication_id,
            pos_id = EXCLUDED.pos_id,
            pos = EXCLUDED.pos,
            form_type = EXCLUDED.form_type,
            content_schema_version = 3,
            form_id = EXCLUDED.form_id,
            variant_id = EXCLUDED.variant_id,
            group_ids = EXCLUDED.group_ids,
            projection_version = EXCLUDED.projection_version,
            updated_at = now()
        "#,
    )
    .bind(source.entry_id)
    .bind(&source.source_id)
    .bind(source.variant_id)
    .bind(v3_kind_string(entry_kind))
    .bind(source.dialect.as_str())
    .bind(source.dialect_scope.as_str())
    .bind(&source.surface)
    .bind(&source.normalized_surface)
    .bind(source.normalization_version)
    .bind(source_revision)
    .bind(event_offset)
    .bind(publication_id)
    .bind(source.pos_id)
    .bind(&source.pos)
    .bind(v3_form_type_name(source.form_type))
    .bind(source.form_id)
    .bind(source.variant_id)
    .bind(&source.group_ids)
    .bind(source.projection_version)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

async fn upsert_v2_publication_surface(
    tx: &mut Transaction<'_, Postgres>,
    source: &crate::lexicon::repository::SurfaceProjectionSource,
    publication_id: Uuid,
    source_revision: i64,
    event_offset: i64,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO lexicon.surface_sources (
            entry_id, source_id, source_kind, source_node_id, language,
            entry_kind, dialect, dialect_scope, surface, normalized_surface,
            normalization_version, source_revision, event_offset, is_deleted,
            content_scope, publication_id, pos_id, pos, form_type,
            content_schema_version, form_id, variant_id, group_ids,
            projection_version, updated_at
        ) VALUES (
            $1, $2, $3, $4, $5,
            $6, $7, $8, $9, $10,
            $11, $12, $13, FALSE,
            'current_publication', $14, $15, $16, $17,
            2, NULL, NULL, NULL,
            NULL, now()
        )
        ON CONFLICT (source_id, content_scope, dialect_scope, normalization_version)
        DO UPDATE SET
            entry_id = EXCLUDED.entry_id,
            source_kind = EXCLUDED.source_kind,
            source_node_id = EXCLUDED.source_node_id,
            language = EXCLUDED.language,
            entry_kind = EXCLUDED.entry_kind,
            dialect = EXCLUDED.dialect,
            surface = EXCLUDED.surface,
            normalized_surface = EXCLUDED.normalized_surface,
            source_revision = EXCLUDED.source_revision,
            event_offset = EXCLUDED.event_offset,
            is_deleted = FALSE,
            publication_id = EXCLUDED.publication_id,
            pos_id = EXCLUDED.pos_id,
            pos = EXCLUDED.pos,
            form_type = EXCLUDED.form_type,
            content_schema_version = 2,
            form_id = NULL,
            variant_id = NULL,
            group_ids = NULL,
            projection_version = NULL,
            updated_at = now()
        "#,
    )
    .bind(source.entry_id)
    .bind(&source.source_id)
    .bind(source.source_kind)
    .bind(source.source_node_id)
    .bind(&source.language)
    .bind(source.entry_kind)
    .bind(source.dialect)
    .bind(source.dialect_scope)
    .bind(&source.surface)
    .bind(&source.normalized_surface)
    .bind(source.normalization_version)
    .bind(source_revision)
    .bind(event_offset)
    .bind(publication_id)
    .bind(source.pos_id)
    .bind(&source.pos)
    .bind(&source.form_type)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

async fn insert_surface_projection_event(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    publication_id: Uuid,
    source_revision: i64,
    event_offset: i64,
    content_schema_version: i16,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO platform.outbox_events (
            id, aggregate_type, aggregate_id, aggregate_revision,
            event_type, payload, occurred_at, available_at
        ) VALUES (
            $1, 'lexicon.surface_projection', $2, $3,
            'lexicon.surface_projection_replaced', $4, now(), now()
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(entry_id)
    .bind(event_offset)
    .bind(serde_json::json!({
        "entry_id": entry_id,
        "content_scope": "current_publication",
        "publication_id": publication_id,
        "source_revision": source_revision,
        "event_offset": event_offset,
        "content_schema_version": content_schema_version,
    }))
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

fn ensure_publication_snapshot_identity(
    publication: &VersionedPublication,
    entry_id: Uuid,
) -> Result<(), LexiconServiceError> {
    let snapshot_entry_id = match publication.content_schema_version {
        2 => {
            serde_json::from_value::<AdminWordV2>(publication.snapshot.clone())
                .map_err(serialization_error)?
                .id
        }
        3 => {
            serde_json::from_value::<AdminWordV3>(publication.snapshot.clone())
                .map_err(serialization_error)?
                .id
        }
        version => return Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
    };
    if publication.entry_id == entry_id && snapshot_entry_id == entry_id {
        Ok(())
    } else {
        Err(invariant_record())
    }
}

async fn insert_v3_publish_event(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    publication: &VersionedPublication,
    source_v2_publication_id: Option<Uuid>,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO platform.outbox_events (
            id, aggregate_type, aggregate_id, aggregate_revision,
            event_type, payload, occurred_at, available_at
        ) VALUES (
            $1, 'lexicon.entry', $2, $3,
            'lexicon.entry_published.v3', $4, $5, $5
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(entry_id)
    .bind(publication.source_revision)
    .bind(serde_json::json!({
        "entry_id": entry_id,
        "publication_id": publication.id,
        "publication_number": publication.publication_number,
        "content_schema_version": 3,
        "source_v2_publication_id": source_v2_publication_id,
    }))
    .bind(publication.published_at)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

async fn insert_v3_activation_event(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    previous_publication_id: Option<Uuid>,
    publication: &VersionedPublication,
    lifecycle_revision: i64,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO platform.outbox_events (
            id, aggregate_type, aggregate_id, aggregate_revision,
            event_type, payload, occurred_at, available_at
        ) VALUES (
            $1, 'lexicon.entry', $2, $3,
            'lexicon.publication_activated.v3', $4, now(), now()
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(entry_id)
    .bind(lifecycle_revision)
    .bind(serde_json::json!({
        "entry_id": entry_id,
        "publication_id": publication.id,
        "publication_number": publication.publication_number,
        "content_schema_version": publication.content_schema_version,
        "previous_publication_id": previous_publication_id,
        "lifecycle_revision": lifecycle_revision,
    }))
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

#[allow(clippy::too_many_arguments)]
async fn insert_v3_command_response(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    actor_id: Uuid,
    request_id: Uuid,
    idempotency_key: Uuid,
    request_hash: &[u8],
    entry_id: Uuid,
    publication_id: Option<Uuid>,
    response_status: i16,
    action: &str,
    metadata: Value,
    response: &AdminWordAnyEnvelope,
) -> Result<(), LexiconServiceError> {
    sqlx::query(
        r#"
        INSERT INTO audit.admin_actions (
            id, actor_admin_id, action, resource_type, resource_id,
            resource_revision, request_id, metadata
        ) VALUES ($1, $2, $3, 'lexicon.entry', $4, $5, $6, $7)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(actor_id)
    .bind(action)
    .bind(entry_id)
    .bind(match &response.word {
        AdminWordAny::V2(word) => word.revision,
        AdminWordAny::V3(word) => word.revision,
    })
    .bind(request_id)
    .bind(metadata)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"
        INSERT INTO platform.idempotency_records (
            scope, idempotency_key, actor_id, request_hash, resource_id,
            response_status, response_body, expires_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, now() + interval '24 hours')
        "#,
    )
    .bind(scope)
    .bind(idempotency_key)
    .bind(actor_id)
    .bind(request_hash)
    .bind(publication_id.or(Some(entry_id)))
    .bind(response_status)
    .bind(serde_json::to_value(response).map_err(serialization_error)?)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}

const fn v3_form_type_name(value: WordFormTypeV3) -> &'static str {
    match value {
        WordFormTypeV3::Base => "base",
        WordFormTypeV3::ThirdPersonSingular => "third_person_singular",
        WordFormTypeV3::PresentParticiple => "present_participle",
        WordFormTypeV3::PastTense => "past_tense",
        WordFormTypeV3::PastParticiple => "past_participle",
        WordFormTypeV3::Plural => "plural",
        WordFormTypeV3::Comparative => "comparative",
        WordFormTypeV3::Superlative => "superlative",
    }
}

#[cfg(test)]
mod eligibility_tests {
    use super::*;

    #[test]
    fn native_publication_requires_absent_migration_provenance() {
        let native = V3PublicationState {
            origin: "native".to_owned(),
            migration_batch_id: None,
            source_publication_id: None,
            publication_canary_enabled: false,
        };
        assert!(ensure_native_publication_state(&native).is_ok());

        for invalid in [
            V3PublicationState {
                migration_batch_id: Some(Uuid::now_v7()),
                ..native.clone()
            },
            V3PublicationState {
                source_publication_id: Some(Uuid::now_v7()),
                ..native.clone()
            },
            V3PublicationState {
                publication_canary_enabled: true,
                ..native.clone()
            },
        ] {
            assert!(ensure_native_publication_state(&invalid).is_err());
        }
    }
}
