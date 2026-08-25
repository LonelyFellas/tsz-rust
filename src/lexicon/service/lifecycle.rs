use super::*;
use chrono::DateTime;

const ARCHIVE_SCOPE: &str = "lexicon.entry.archive";
const RESTORE_SCOPE: &str = "lexicon.entry.restore";
const ARCHIVE_BATCH_SCOPE: &str = "lexicon.entry.archive_batch";
const RESTORE_BATCH_SCOPE: &str = "lexicon.entry.restore_batch";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetState {
    Active,
    Archived,
}

impl TargetState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
}

impl LexiconService {
    pub async fn lifecycle_contains_v3(
        &self,
        entry_ids: &[Uuid],
    ) -> Result<bool, LexiconServiceError> {
        let versions = self
            .repository
            .lifecycle_schema_versions(entry_ids)
            .await
            .map_err(repository_error)?;
        if let Some(version) = versions.iter().find(|version| !matches!(version, 2 | 3)) {
            return Err(LexiconServiceError::UnsupportedSchemaVersion(*version));
        }
        Ok(versions.contains(&3))
    }

    pub async fn delete_draft(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        input: DeleteDraftInput,
        allow_v3: bool,
    ) -> Result<(), LexiconServiceError> {
        if input.base_revision < 1 {
            return Err(LexiconServiceError::UnprocessableField {
                field: "base_revision",
                message: "base_revision must be at least 1",
            });
        }
        if input.base_lifecycle_revision < 1 {
            return Err(LexiconServiceError::UnprocessableField {
                field: "base_lifecycle_revision",
                message: "base_lifecycle_revision must be at least 1",
            });
        }
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        LexiconRepository::lock_surface_contexts(&mut transaction, &[entry_id])
            .await
            .map_err(repository_error)?;
        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        if record.revision != input.base_revision {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        if record.lifecycle_revision != input.base_lifecycle_revision {
            return Err(LexiconServiceError::LifecycleRevisionConflict {
                current_lifecycle_revision: record.lifecycle_revision,
            });
        }
        if !matches!(record.content_schema_version, 2 | 3) {
            return Err(LexiconServiceError::UnsupportedSchemaVersion(
                record.content_schema_version,
            ));
        }
        ensure_lifecycle_schema_capability(record.content_schema_version, allow_v3)?;
        let relational_meanings: DraftMeaningsStepContent =
            serde_json::from_value(record.meanings.clone()).map_err(serialization_error)?;
        let relation_targets = relation_target_entry_ids(&relational_meanings);
        LexiconRepository::lock_surface_contexts(&mut transaction, &relation_targets)
            .await
            .map_err(repository_error)?;
        let surface_sources = if record.content_schema_version == 2 {
            let current = entry_from_record(record)?;
            crate::lexicon::repository::surface_projection_sources(&current)
                .map_err(surface_projection_error)?
        } else {
            Vec::new()
        };
        let mut surface_keys =
            crate::lexicon::repository::surface_lock_keys([surface_sources.as_slice()]);
        if surface_sources.is_empty() {
            surface_keys.extend(
                LexiconRepository::lifecycle_surface_lock_keys(&mut transaction, &[entry_id])
                    .await
                    .map_err(repository_error)?,
            );
        }
        LexiconRepository::lock_surface_keys(&mut transaction, &surface_keys)
            .await
            .map_err(repository_error)?;
        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        if record.archived_at.is_some() || record.current_publication_id.is_some() {
            return Err(LexiconServiceError::EntryNotDeletable);
        }
        if record.content_schema_version == 2 {
            LexiconRepository::replace_surface_projection(
                &mut transaction,
                entry_id,
                record.revision + 1,
                crate::lexicon::repository::SurfaceContentScope::Draft,
                None,
                &surface_sources,
                &[],
            )
            .await
            .map_err(repository_error)?;
        } else {
            LexiconRepository::retire_v3_draft_surface_projection(
                &mut transaction,
                entry_id,
                record.revision + 1,
            )
            .await
            .map_err(repository_error)?;
        }
        if !LexiconRepository::delete_never_published_entry(
            &mut transaction,
            actor_id,
            request_id,
            entry_id,
            record.revision,
        )
        .await
        .map_err(repository_error)?
        {
            return Err(LexiconServiceError::EntryNotDeletable);
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }

    pub async fn archive(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        idempotency_key: Uuid,
        input: EntryLifecycleInput,
        allow_v3: bool,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        let confirmed_surface_match_token = input.confirmed_surface_match_token.clone();
        let response = self
            .transition_lifecycle(
                actor_id,
                request_id,
                idempotency_key,
                ARCHIVE_SCOPE,
                TargetState::Archived,
                vec![single_target(entry_id, input)],
                confirmed_surface_match_token.as_deref(),
                allow_v3,
            )
            .await?;
        one_word(response)
    }

    pub async fn restore(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        idempotency_key: Uuid,
        input: EntryLifecycleInput,
        allow_v3: bool,
    ) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
        let confirmed_surface_match_token = input.confirmed_surface_match_token.clone();
        let response = self
            .transition_lifecycle(
                actor_id,
                request_id,
                idempotency_key,
                RESTORE_SCOPE,
                TargetState::Active,
                vec![single_target(entry_id, input)],
                confirmed_surface_match_token.as_deref(),
                allow_v3,
            )
            .await?;
        one_word(response)
    }

    pub async fn archive_batch(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        idempotency_key: Uuid,
        input: EntryLifecycleBatchInput,
        allow_v3: bool,
    ) -> Result<EntryLifecycleBatchResponseAny, LexiconServiceError> {
        let confirmed_surface_match_token = input.confirmed_surface_match_token.clone();
        self.transition_lifecycle(
            actor_id,
            request_id,
            idempotency_key,
            ARCHIVE_BATCH_SCOPE,
            TargetState::Archived,
            input.entries,
            confirmed_surface_match_token.as_deref(),
            allow_v3,
        )
        .await
    }

    pub async fn restore_batch(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        idempotency_key: Uuid,
        input: EntryLifecycleBatchInput,
        allow_v3: bool,
    ) -> Result<EntryLifecycleBatchResponseAny, LexiconServiceError> {
        let confirmed_surface_match_token = input.confirmed_surface_match_token.clone();
        self.transition_lifecycle(
            actor_id,
            request_id,
            idempotency_key,
            RESTORE_BATCH_SCOPE,
            TargetState::Active,
            input.entries,
            confirmed_surface_match_token.as_deref(),
            allow_v3,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn transition_lifecycle(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        idempotency_key: Uuid,
        scope: &'static str,
        target_state: TargetState,
        targets: Vec<EntryLifecycleTarget>,
        confirmed_surface_match_token: Option<&str>,
        allow_v3: bool,
    ) -> Result<EntryLifecycleBatchResponseAny, LexiconServiceError> {
        validate_targets(&targets)?;
        let request_hash = sha256_json(&serde_json::json!({
            "target_state": target_state.as_str(),
            "entries": &targets,
        }))
        .map_err(serialization_error)?;
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{scope}:{actor_id}:{idempotency_key}"))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if let Some(existing) =
            LexiconRepository::idempotency(&mut transaction, scope, actor_id, idempotency_key)
                .await
                .map_err(repository_error)?
        {
            if existing.request_hash != request_hash {
                return Err(LexiconServiceError::IdempotencyConflict);
            }
            transaction.commit().await.map_err(database_error)?;
            return serde_json::from_value(existing.response_body).map_err(serialization_error);
        }

        let target_entry_ids = targets.iter().map(|target| target.id).collect::<Vec<_>>();
        // Publication and activation writers take the entry surface-context
        // lock before the aggregate row. Join that order before reading any
        // target FOR UPDATE so lifecycle cannot form context <-> row ABBA.
        LexiconRepository::lock_surface_contexts(&mut transaction, &target_entry_ids)
            .await
            .map_err(repository_error)?;
        let excluded_sources = if target_state == TargetState::Archived {
            targets.iter().map(|target| target.id).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let restoring_entries = if target_state == TargetState::Active {
            targets.iter().map(|target| target.id).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut sorted = targets.clone();
        sorted.sort_by_key(|target| target.id);
        let mut pending = Vec::with_capacity(sorted.len());
        for target in sorted {
            let record = LexiconRepository::entry_by_id_for_update(&mut transaction, target.id)
                .await
                .map_err(repository_error)?
                .ok_or(LexiconServiceError::WordNotFound)?;
            ensure_lifecycle_schema_capability(record.content_schema_version, allow_v3)?;
            let current = match record.content_schema_version {
                2 => AdminWordAny::V2(Box::new(entry_from_record(record)?)),
                3 => {
                    let record_revision = record.revision;
                    let record_lifecycle_revision = record.lifecycle_revision;
                    let word = self.get_v3(record.id).await?;
                    if word.revision != record_revision
                        || word.lifecycle_revision != record_lifecycle_revision
                    {
                        return Err(invariant_record());
                    }
                    AdminWordAny::V3(Box::new(word))
                }
                version => return Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
            };
            ensure_any_revision(&current, target.base_revision)?;
            let already_target = match target_state {
                TargetState::Archived => any_archived_at(&current).is_some(),
                TargetState::Active => any_archived_at(&current).is_none(),
            };
            if !already_target {
                ensure_any_lifecycle_revision(&current, target.base_lifecycle_revision)?;
            }
            pending.push((target, current, already_target));
        }

        let pending_entry_ids = pending
            .iter()
            .map(|(_, word, _)| any_id(word))
            .collect::<Vec<_>>();
        let mut affected_contexts = pending
            .iter()
            .map(|(_, word, _)| any_relation_target_entry_ids(word))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        affected_contexts.extend(
            LexiconRepository::current_publication_relation_target_entry_ids(
                &mut transaction,
                &pending_entry_ids,
            )
            .await
            .map_err(repository_error)?,
        );
        LexiconRepository::lock_surface_contexts(&mut transaction, &affected_contexts)
            .await
            .map_err(repository_error)?;
        LexiconRepository::lock_surface_policy_writer(&mut transaction)
            .await
            .map_err(repository_error)?;

        let surface_sets = pending
            .iter()
            .filter_map(|(_, word, _)| match word {
                AdminWordAny::V2(word) => Some(
                    crate::lexicon::repository::surface_projection_sources(word)
                        .map_err(surface_projection_error),
                ),
                AdminWordAny::V3(_) => None,
            })
            .collect::<Result<Vec<_>, _>>()?;
        let v3_entry_ids = pending
            .iter()
            .filter(|(_, word, _)| matches!(word, AdminWordAny::V3(_)))
            .map(|(_, word, _)| any_id(word))
            .collect::<Vec<_>>();
        let entry_ids = pending
            .iter()
            .filter(|(_, _, already_target)| {
                target_state == TargetState::Archived || !*already_target
            })
            .map(|(_, word, _)| any_id(word))
            .collect::<Vec<_>>();
        let v2_entry_ids = entry_ids
            .iter()
            .copied()
            .filter(|entry_id| {
                pending.iter().any(|(_, word, _)| {
                    any_id(word) == *entry_id && matches!(word, AdminWordAny::V2(_))
                })
            })
            .collect::<Vec<_>>();
        let publication_sources =
            LexiconRepository::current_publication_surface_sources(&mut transaction, &v2_entry_ids)
                .await
                .map_err(repository_error)?;
        let mut surface_keys = crate::lexicon::repository::surface_lock_keys(
            surface_sets
                .iter()
                .map(Vec::as_slice)
                .chain(std::iter::once(publication_sources.as_slice())),
        );
        for (_, word, _) in &pending {
            let AdminWordAny::V3(word) = word else {
                continue;
            };
            let projected =
                crate::lexicon::v3_projection::form_variant_sources(word.id, &word.forms)
                    .map_err(|_| invariant_record())?;
            surface_keys.extend(projected.into_iter().map(|source| {
                crate::lexicon::repository::SurfaceLockKey {
                    language: "en".to_owned(),
                    dialect_scope: source.dialect_scope.as_str().to_owned(),
                    normalized_surface: source.normalized_surface,
                }
            }));
        }
        surface_keys.extend(
            LexiconRepository::lifecycle_surface_lock_keys(&mut transaction, &v3_entry_ids)
                .await
                .map_err(repository_error)?,
        );
        surface_keys.sort();
        surface_keys.dedup();
        LexiconRepository::lock_surface_keys(&mut transaction, &surface_keys)
            .await
            .map_err(repository_error)?;
        let verified_visibility = if target_state == TargetState::Active {
            self.confirm_restore_visibility(
                &mut transaction,
                actor_id,
                scope,
                &pending,
                &targets,
                &publication_sources,
                confirmed_surface_match_token,
            )
            .await?
        } else {
            None
        };

        let mut words_by_id = HashMap::new();
        let mut restored_audit_targets = Vec::new();
        let mut affected = 0;
        for (_target, current, already_target) in pending {
            if already_target {
                words_by_id.insert(any_id(&current), current);
                continue;
            }
            if target_state == TargetState::Archived {
                let references = LexiconRepository::active_inbound_sense_refs(
                    &mut transaction,
                    any_id(&current),
                    &excluded_sources,
                )
                .await
                .map_err(repository_error)?;
                if !references.is_empty() {
                    return Err(LexiconServiceError::EntryHasInboundPublicationRefs(
                        references,
                    ));
                }
            } else {
                LexiconRepository::lock_current_outbound_sense_ref_targets_for_entry(
                    &mut transaction,
                    any_id(&current),
                )
                .await
                .map_err(repository_error)?;
                let references = LexiconRepository::unavailable_outbound_sense_refs_for_restore(
                    &mut transaction,
                    any_id(&current),
                    &restoring_entries,
                )
                .await
                .map_err(repository_error)?;
                if !references.is_empty() {
                    return Err(LexiconServiceError::EntryHasUnavailablePublicationRefs(
                        references,
                    ));
                }
            }
            let next = lifecycle_word_any(current, target_state, actor_id);
            LexiconRepository::transition_lifecycle(&mut transaction, &next, actor_id, request_id)
                .await
                .map_err(repository_error)?;
            if target_state == TargetState::Active {
                restored_audit_targets.push((any_id(&next), any_revision(&next)));
            }
            affected += 1;
            words_by_id.insert(any_id(&next), next);
        }
        let response = EntryLifecycleBatchResponseAny {
            words: targets
                .iter()
                .filter_map(|target| words_by_id.remove(&target.id))
                .collect(),
            affected,
        };
        if response.words.len() != targets.len() {
            return Err(invariant_record());
        }
        if let Some(confirmation) = verified_visibility.as_ref() {
            for (entry_id, revision) in restored_audit_targets {
                LexiconRepository::insert_command_surface_confirmation_audits(
                    &mut transaction,
                    actor_id,
                    request_id,
                    entry_id,
                    revision,
                    confirmation,
                )
                .await
                .map_err(repository_error)?;
            }
        }
        LexiconRepository::insert_idempotent_response(
            &mut transaction,
            scope,
            actor_id,
            idempotency_key,
            &request_hash,
            (targets.len() == 1).then_some(targets[0].id),
            &response,
            200,
        )
        .await
        .map_err(repository_error)?;
        transaction.commit().await.map_err(database_error)?;
        if let Some(confirmation) = verified_visibility
            && let Err(error) = self.surface_snapshots.remove_verified(&confirmation).await
        {
            tracing::warn!(
                ?error,
                "failed to remove consumed restore visibility snapshot"
            );
        }
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    async fn confirm_restore_visibility(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        actor_id: Uuid,
        scope: &'static str,
        pending: &[(EntryLifecycleTarget, AdminWordAny, bool)],
        all_targets: &[EntryLifecycleTarget],
        publication_sources: &[crate::lexicon::repository::SurfaceProjectionSource],
        token: Option<&str>,
    ) -> Result<Option<VerifiedSurfaceConfirmation>, LexiconServiceError> {
        let contains_v3 = pending
            .iter()
            .any(|(_, word, _)| matches!(word, AdminWordAny::V3(_)));
        let additions = crate::lexicon::visibility::headword_memberships(publication_sources);
        let mut requested = additions
            .iter()
            .map(|(scope, _)| scope.clone())
            .collect::<Vec<_>>();
        requested.sort();
        requested.dedup();
        let before =
            LexiconRepository::active_headword_memberships_in_transaction(transaction, &requested)
                .await
                .map_err(repository_error)?;
        let transitions = crate::lexicon::visibility::transitions(before, [], additions);
        let visibility_required =
            crate::lexicon::visibility::requires_multiple_active_confirmation(&transitions);
        let selection = all_targets
            .iter()
            .map(|target| {
                serde_json::json!({
                    "word_id": target.id,
                    "base_revision": target.base_revision,
                    "base_lifecycle_revision": target.base_lifecycle_revision,
                })
            })
            .collect::<Vec<_>>();
        let active_ids = transitions
            .iter()
            .flat_map(|item| item.after_active_ids.iter().copied())
            .collect::<std::collections::HashSet<_>>();
        let mut items = std::collections::BTreeMap::new();
        let mut contexts = std::collections::BTreeMap::new();
        for (_, word, already_target) in pending {
            let AdminWordAny::V2(word) = word else {
                continue;
            };
            if *already_target {
                continue;
            }
            let (mut headword_items, headword_contexts) = self
                .headword_surface_matches_in_transaction(
                    transaction,
                    &word.headwords,
                    word.kind,
                    Some(word.id),
                )
                .await?;
            for item in &mut headword_items {
                if let SurfaceMatchCandidateV2::Headword {
                    candidate_word_id, ..
                } = &mut item.candidate
                {
                    *candidate_word_id = Some(word.id);
                }
            }
            let (form_items, form_contexts) = self
                .form_surface_matches_in_transaction(transaction, word)
                .await?;
            let headword_evidence =
                LexiconRepository::headword_surface_acknowledgement(transaction, word.id)
                    .await
                    .map_err(repository_error)?;
            let forms_evidence =
                LexiconRepository::forms_surface_acknowledgement(transaction, word.id)
                    .await
                    .map_err(repository_error)?;
            let acknowledged_headwords = self
                .valid_headword_acknowledgement_ids(word, headword_evidence.as_ref())
                .await?;
            let acknowledged_forms = self
                .valid_forms_acknowledgement_ids(word, forms_evidence.as_ref())
                .await?;
            let snapshot_id = |match_id: &str| {
                if scope == RESTORE_BATCH_SCOPE {
                    format!("{}:{match_id}", word.id)
                } else {
                    match_id.to_owned()
                }
            };
            for mut item in headword_items.iter().chain(form_items.iter()).cloned() {
                let acknowledged = match item.candidate {
                    SurfaceMatchCandidateV2::Headword { .. } => &acknowledged_headwords,
                    SurfaceMatchCandidateV2::Form { .. } => &acknowledged_forms,
                };
                if acknowledged.contains(item.match_id.as_str()) {
                    continue;
                }
                item.match_id = snapshot_id(&item.match_id);
                item.confirmation_reasons =
                    vec![SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches];
                items.insert(item.match_id.clone(), item);
            }
            if visibility_required {
                for mut item in headword_items {
                    if !active_ids.contains(&item.existing.word_id)
                        || !matches!(
                            item.existing.source,
                            ExistingSurfaceSourceV2::Headword {
                                content_scope: SurfaceContentScopeV2::CurrentPublication,
                                ..
                            }
                        )
                    {
                        continue;
                    }
                    item.match_id = snapshot_id(&item.match_id);
                    match items.entry(item.match_id.clone()) {
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            item.confirmation_reasons =
                                vec![SurfaceConfirmationReasonV2::VisibilityActivation];
                            entry.insert(item);
                        }
                        std::collections::btree_map::Entry::Occupied(mut entry) => {
                            entry.get_mut().confirmation_reasons = vec![
                                SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches,
                                SurfaceConfirmationReasonV2::VisibilityActivation,
                            ];
                        }
                    }
                }
            }
            for context in headword_contexts.into_iter().chain(form_contexts) {
                if items
                    .values()
                    .any(|item| item.existing.word_id == context.word_id)
                {
                    contexts.insert(context.word_id, context);
                }
            }
        }
        let v2_publication_contribution = self
            .v2_restore_publication_surface_contribution(transaction, pending, publication_sources)
            .await?;
        for mut item in v2_publication_contribution.items {
            if visibility_required
                && active_ids.contains(&item.existing.word_id)
                && matches!(
                    &item.existing.source,
                    ExistingSurfaceSourceV2::Headword {
                        content_scope: SurfaceContentScopeV2::CurrentPublication,
                        ..
                    }
                )
            {
                item.confirmation_reasons = vec![
                    SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches,
                    SurfaceConfirmationReasonV2::VisibilityActivation,
                ];
            }
            if items.insert(item.match_id.clone(), item).is_some() {
                return Err(invariant_record());
            }
        }
        for context in v2_publication_contribution.contexts {
            contexts.entry(context.word_id).or_insert(context);
        }
        let v3_contribution = if contains_v3 {
            Some(
                self.v3_restore_surface_contribution(transaction, pending)
                    .await?,
            )
        } else {
            None
        };
        if let Some(contribution) = &v3_contribution {
            for item in &contribution.items {
                if items.insert(item.match_id.clone(), item.clone()).is_some() {
                    return Err(invariant_record());
                }
            }
        }
        let items = items.into_values().collect::<Vec<_>>();
        let command = if scope == RESTORE_SCOPE {
            SurfaceConsumptionCommand::RestoreEntry
        } else {
            SurfaceConsumptionCommand::RestoreEntriesBatch
        };
        let owner_context = serde_json::to_string(&selection).map_err(serialization_error)?;
        if items.is_empty() {
            if contains_v3 && let Some(token) = token {
                self.verify_v3_surface_owner(token, actor_id, command, owner_context)
                    .await?;
                return Err(LexiconServiceError::SurfaceMatchesChangedWithoutSnapshot);
            }
            return Ok(None);
        }
        let v3_page_data = if let Some(contribution) = &v3_contribution {
            Some(
                self.v3_restore_page_data(transaction, &items, &contribution.page_items)
                    .await?,
            )
        } else {
            None
        };
        let contexts = if let Some(page_data) = &v3_page_data {
            super::v3_surface::v3_restore_synthetic_contexts(page_data)
        } else {
            contexts.into_values().collect::<Vec<_>>()
        };
        let policy = if visibility_required {
            self.surface_policies
                .multiple_active_exact_headword_publications()
                .await
        } else {
            self.surface_policies
                .policy(SurfacePolicyNameV2::SurfaceWarningAcknowledgement)
                .await
        }
        .map_err(LexiconServiceError::SurfacePolicy)?;
        let confirmation_reasons = if visibility_required {
            if items.iter().any(|item| {
                item.confirmation_reasons
                    .contains(&SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches)
            }) {
                vec![
                    SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches,
                    SurfaceConfirmationReasonV2::VisibilityActivation,
                ]
            } else {
                vec![SurfaceConfirmationReasonV2::VisibilityActivation]
            }
        } else {
            vec![SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches]
        };
        let v2_publication_entry_ids = pending
            .iter()
            .filter_map(|(_, word, already_target)| {
                (!*already_target && matches!(word, AdminWordAny::V2(_))).then_some(any_id(word))
            })
            .collect::<Vec<_>>();
        let v2_current_publication_surface_evidence =
            LexiconRepository::lifecycle_v2_publication_surface_evidence(
                transaction,
                &v2_publication_entry_ids,
            )
            .await
            .map_err(repository_error)?;
        let entry_state_evidence = pending
            .iter()
            .map(|(_, word, already_target)| match word {
                AdminWordAny::V2(word) => serde_json::json!({
                    "schema_version": 2,
                    "entry_id": word.id,
                    "current_revision": word.revision,
                    "current_lifecycle_revision": word.lifecycle_revision,
                    "published_revision": word.published_revision,
                    "already_active": already_target,
                }),
                AdminWordAny::V3(word) => serde_json::json!({
                    "schema_version": 3,
                    "entry_id": word.id,
                    "current_revision": word.revision,
                    "current_lifecycle_revision": word.lifecycle_revision,
                    "published_revision": word.published_revision,
                    "already_active": already_target,
                }),
            })
            .collect::<Vec<_>>();
        let mut owner_bundle = serde_json::json!({
            "command": if scope == RESTORE_SCOPE { "restore_entry" } else { "restore_entries_batch" },
            "selection": selection,
            "entry_state_evidence": entry_state_evidence,
            "transitions": transitions,
            "match_ids": items.iter().map(|item| &item.match_id).collect::<Vec<_>>(),
            "confirmation_reasons": confirmation_reasons,
            "v2_current_publication_surface_evidence": v2_current_publication_surface_evidence,
        });
        if let (Some(page_data), Some(contribution)) = (&v3_page_data, &v3_contribution) {
            owner_bundle[crate::lexicon::surface_snapshot::V3_SURFACE_PAGE_DATA_KEY] =
                serde_json::to_value(page_data).map_err(serialization_error)?;
            owner_bundle["v3_candidate_evidence"] =
                serde_json::to_value(&contribution.candidate_evidence)
                    .map_err(serialization_error)?;
        }
        let owner_digest =
            surface_owner_bundle_digest(&owner_bundle).map_err(serialization_error)?;
        let binding = SurfaceConfirmationBinding {
            actor_id,
            command,
            owner_context,
            base_revision: None,
            canonical_content_digest: owner_digest.clone(),
            owner_evidence_digest: owner_digest,
            normalization_version: crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION,
            policy_name: policy.name,
            policy_epoch: policy.epoch,
        };
        let create_snapshot = || CreateSurfaceSnapshot {
            binding: binding.clone(),
            policy_enabled: policy.enabled,
            policy_block_code: (!policy.enabled && visibility_required).then_some(
                SurfacePolicyBlockCodeV2::MultipleActiveExactHeadwordPublicationsNotEnabled,
            ),
            items: items.clone(),
            matched_entry_contexts: contexts.clone(),
            confirmation_reasons: confirmation_reasons.clone(),
            owner_bundle: owner_bundle.clone(),
            page_size: DEFAULT_SURFACE_PAGE_SIZE,
        };
        let Some(token) = token else {
            let snapshot = self
                .surface_snapshots
                .create(create_snapshot())
                .await
                .map_err(LexiconServiceError::SurfaceSnapshot)?;
            if contains_v3 {
                let page =
                    crate::lexicon::surface_snapshot::surface_page_v3(snapshot.page, &owner_bundle)
                        .map_err(LexiconServiceError::SurfaceSnapshot)?;
                if !policy.enabled && visibility_required {
                    return Err(
                        LexiconServiceError::MultipleActiveExactHeadwordPublicationsNotEnabledV3(
                            Box::new(page),
                        ),
                    );
                }
                return Err(LexiconServiceError::SurfaceMatchAcknowledgementRequiredV3(
                    Box::new(page),
                ));
            }
            if !policy.enabled && visibility_required {
                return Err(
                    LexiconServiceError::MultipleActiveExactHeadwordPublicationsNotEnabled(
                        Box::new(snapshot.page),
                    ),
                );
            }
            return Err(LexiconServiceError::SurfaceMatchAcknowledgementRequired(
                Box::new(snapshot.page),
            ));
        };
        let expected = ExpectedSurfaceConfirmation {
            binding: binding.clone(),
            current_policy: policy,
        };
        let verified = match self.surface_snapshots.verify(token, &expected).await {
            Ok(verified) => verified,
            Err(SurfaceSnapshotError::Expired) => {
                return Err(LexiconServiceError::SurfaceMatchSnapshotExpired);
            }
            Err(SurfaceSnapshotError::PolicyChanged(name)) => {
                let current = self
                    .surface_policies
                    .policy(name)
                    .await
                    .map_err(LexiconServiceError::SurfacePolicy)?;
                return Err(LexiconServiceError::SurfacePolicyChanged(current));
            }
            Err(SurfaceSnapshotError::BindingMismatch) => {
                let snapshot = self
                    .surface_snapshots
                    .create(create_snapshot())
                    .await
                    .map_err(LexiconServiceError::SurfaceSnapshot)?;
                if contains_v3 {
                    let page = crate::lexicon::surface_snapshot::surface_page_v3(
                        snapshot.page,
                        &owner_bundle,
                    )
                    .map_err(LexiconServiceError::SurfaceSnapshot)?;
                    return Err(LexiconServiceError::SurfaceMatchesChangedV3(Box::new(page)));
                }
                return Err(LexiconServiceError::SurfaceMatchesChanged(Box::new(
                    snapshot.page,
                )));
            }
            Err(error) => return Err(LexiconServiceError::SurfaceSnapshot(error)),
        };
        if !policy.enabled && visibility_required {
            let snapshot = self
                .surface_snapshots
                .create(create_snapshot())
                .await
                .map_err(LexiconServiceError::SurfaceSnapshot)?;
            if contains_v3 {
                let page =
                    crate::lexicon::surface_snapshot::surface_page_v3(snapshot.page, &owner_bundle)
                        .map_err(LexiconServiceError::SurfaceSnapshot)?;
                return Err(
                    LexiconServiceError::MultipleActiveExactHeadwordPublicationsNotEnabledV3(
                        Box::new(page),
                    ),
                );
            }
            return Err(
                LexiconServiceError::MultipleActiveExactHeadwordPublicationsNotEnabled(Box::new(
                    snapshot.page,
                )),
            );
        }
        let current_ids = items
            .iter()
            .map(|item| item.match_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let verified_ids = verified
            .match_ids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        let current_digest =
            crate::lexicon::surface_snapshot::surface_match_digest(&items, &confirmation_reasons)
                .map_err(LexiconServiceError::SurfaceSnapshot)?;
        let current_context_digest =
            surface_context_digest(&contexts).map_err(LexiconServiceError::SurfaceSnapshot)?;
        if current_ids != verified_ids
            || current_digest != verified.match_digest
            || current_context_digest != verified.context_digest
        {
            let snapshot = self
                .surface_snapshots
                .create(create_snapshot())
                .await
                .map_err(LexiconServiceError::SurfaceSnapshot)?;
            if contains_v3 {
                let page =
                    crate::lexicon::surface_snapshot::surface_page_v3(snapshot.page, &owner_bundle)
                        .map_err(LexiconServiceError::SurfaceSnapshot)?;
                return Err(LexiconServiceError::SurfaceMatchesChangedV3(Box::new(page)));
            }
            return Err(LexiconServiceError::SurfaceMatchesChanged(Box::new(
                snapshot.page,
            )));
        }
        Ok(Some(verified))
    }
}

fn single_target(id: Uuid, input: EntryLifecycleInput) -> EntryLifecycleTarget {
    EntryLifecycleTarget {
        id,
        base_revision: input.base_revision,
        base_lifecycle_revision: input.base_lifecycle_revision,
    }
}

fn one_word(
    response: EntryLifecycleBatchResponseAny,
) -> Result<AdminWordAnyEnvelope, LexiconServiceError> {
    Ok(AdminWordAnyEnvelope {
        word: response
            .words
            .into_iter()
            .next()
            .ok_or_else(invariant_record)?,
    })
}

fn validate_targets(targets: &[EntryLifecycleTarget]) -> Result<(), LexiconServiceError> {
    if targets.is_empty() || targets.len() > 100 {
        return Err(semantic(
            "entries",
            "entries must contain between 1 and 100 values",
        ));
    }
    if targets
        .iter()
        .any(|target| target.base_revision < 1 || target.base_lifecycle_revision < 1)
    {
        return Err(semantic("entries", "entry revisions must be at least 1"));
    }
    let mut ids = std::collections::HashSet::new();
    if targets.iter().any(|target| !ids.insert(target.id)) {
        return Err(semantic(
            "entries",
            "entry ids must be unique within one request",
        ));
    }
    Ok(())
}

fn ensure_lifecycle_schema_capability(
    content_schema_version: i16,
    allow_v3: bool,
) -> Result<(), LexiconServiceError> {
    if content_schema_version == 3 && !allow_v3 {
        return Err(LexiconServiceError::V3StorageUnavailable);
    }
    Ok(())
}

fn lifecycle_word_any(
    mut word: AdminWordAny,
    target_state: TargetState,
    actor_id: Uuid,
) -> AdminWordAny {
    let now = Utc::now();
    match &mut word {
        AdminWordAny::V2(word) => {
            word.lifecycle_revision += 1;
            word.updated_at = now;
            apply_lifecycle_state(
                &mut word.status,
                &mut word.archived_at,
                &mut word.archived_by,
                word.published_revision,
                target_state,
                actor_id,
                now,
            );
        }
        AdminWordAny::V3(word) => {
            word.lifecycle_revision += 1;
            word.updated_at = now;
            apply_lifecycle_state(
                &mut word.status,
                &mut word.archived_at,
                &mut word.archived_by,
                word.published_revision,
                target_state,
                actor_id,
                now,
            );
        }
    }
    word
}

#[allow(clippy::too_many_arguments)]
fn apply_lifecycle_state(
    status: &mut AdminWordStatus,
    archived_at: &mut Option<DateTime<Utc>>,
    archived_by: &mut Option<Uuid>,
    published_revision: Option<i64>,
    target_state: TargetState,
    actor_id: Uuid,
    now: DateTime<Utc>,
) {
    match target_state {
        TargetState::Archived => {
            *status = AdminWordStatus::Archived;
            *archived_at = Some(now);
            *archived_by = Some(actor_id);
        }
        TargetState::Active => {
            *status = if published_revision.is_some() {
                AdminWordStatus::Published
            } else {
                AdminWordStatus::Draft
            };
            *archived_at = None;
            *archived_by = None;
        }
    }
}

fn any_id(word: &AdminWordAny) -> Uuid {
    match word {
        AdminWordAny::V2(word) => word.id,
        AdminWordAny::V3(word) => word.id,
    }
}

fn any_revision(word: &AdminWordAny) -> i64 {
    match word {
        AdminWordAny::V2(word) => word.revision,
        AdminWordAny::V3(word) => word.revision,
    }
}

fn any_lifecycle_revision(word: &AdminWordAny) -> i64 {
    match word {
        AdminWordAny::V2(word) => word.lifecycle_revision,
        AdminWordAny::V3(word) => word.lifecycle_revision,
    }
}

fn any_archived_at(word: &AdminWordAny) -> Option<&DateTime<Utc>> {
    match word {
        AdminWordAny::V2(word) => word.archived_at.as_ref(),
        AdminWordAny::V3(word) => word.archived_at.as_ref(),
    }
}

fn ensure_any_revision(word: &AdminWordAny, base_revision: i64) -> Result<(), LexiconServiceError> {
    if any_revision(word) != base_revision {
        return Err(LexiconServiceError::RevisionConflict {
            current_revision: any_revision(word),
        });
    }
    Ok(())
}

fn ensure_any_lifecycle_revision(
    word: &AdminWordAny,
    base_lifecycle_revision: i64,
) -> Result<(), LexiconServiceError> {
    if any_lifecycle_revision(word) != base_lifecycle_revision {
        return Err(LexiconServiceError::LifecycleRevisionConflict {
            current_lifecycle_revision: any_lifecycle_revision(word),
        });
    }
    Ok(())
}

fn any_relation_target_entry_ids(word: &AdminWordAny) -> Result<Vec<Uuid>, LexiconServiceError> {
    match word {
        AdminWordAny::V2(word) => Ok(relation_target_entry_ids(&word.meanings)),
        AdminWordAny::V3(word) => {
            let meanings: DraftMeaningsStepContent = serde_json::from_value(
                serde_json::to_value(&word.meanings).map_err(serialization_error)?,
            )
            .map_err(serialization_error)?;
            Ok(relation_target_entry_ids(&meanings))
        }
    }
}

pub(super) fn ensure_lifecycle_revision(
    word: &AdminWordV2,
    base_lifecycle_revision: i64,
) -> Result<(), LexiconServiceError> {
    if word.lifecycle_revision != base_lifecycle_revision {
        return Err(LexiconServiceError::LifecycleRevisionConflict {
            current_lifecycle_revision: word.lifecycle_revision,
        });
    }
    Ok(())
}

fn semantic(field: &'static str, message: &'static str) -> LexiconServiceError {
    LexiconServiceError::UnprocessableField { field, message }
}
