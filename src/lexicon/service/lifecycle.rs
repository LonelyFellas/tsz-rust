use super::*;

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
    pub async fn delete_draft(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        input: DeleteDraftInput,
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
        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        let current = entry_from_record(record)?;
        if current.revision != input.base_revision {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: current.revision,
            });
        }
        if current.lifecycle_revision != input.base_lifecycle_revision {
            return Err(LexiconServiceError::LifecycleRevisionConflict {
                current_lifecycle_revision: current.lifecycle_revision,
            });
        }
        let surface_sources = crate::lexicon::repository::surface_projection_sources(&current)
            .map_err(surface_projection_error)?;
        let surface_keys =
            crate::lexicon::repository::surface_lock_keys([surface_sources.as_slice()]);
        LexiconRepository::lock_surface_keys(&mut transaction, &surface_keys)
            .await
            .map_err(repository_error)?;
        if current.archived_at.is_some() || current.published_revision.is_some() {
            return Err(LexiconServiceError::EntryNotDeletable);
        }
        LexiconRepository::replace_surface_projection(
            &mut transaction,
            entry_id,
            current.revision + 1,
            crate::lexicon::repository::SurfaceContentScope::Draft,
            None,
            &surface_sources,
            &[],
        )
        .await
        .map_err(repository_error)?;
        if !LexiconRepository::delete_never_published_entry(
            &mut transaction,
            actor_id,
            request_id,
            entry_id,
            current.revision,
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
    ) -> Result<AdminWordV2Envelope, LexiconServiceError> {
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
    ) -> Result<AdminWordV2Envelope, LexiconServiceError> {
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
    ) -> Result<EntryLifecycleBatchResponse, LexiconServiceError> {
        let confirmed_surface_match_token = input.confirmed_surface_match_token.clone();
        self.transition_lifecycle(
            actor_id,
            request_id,
            idempotency_key,
            ARCHIVE_BATCH_SCOPE,
            TargetState::Archived,
            input.entries,
            confirmed_surface_match_token.as_deref(),
        )
        .await
    }

    pub async fn restore_batch(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        idempotency_key: Uuid,
        input: EntryLifecycleBatchInput,
    ) -> Result<EntryLifecycleBatchResponse, LexiconServiceError> {
        let confirmed_surface_match_token = input.confirmed_surface_match_token.clone();
        self.transition_lifecycle(
            actor_id,
            request_id,
            idempotency_key,
            RESTORE_BATCH_SCOPE,
            TargetState::Active,
            input.entries,
            confirmed_surface_match_token.as_deref(),
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
    ) -> Result<EntryLifecycleBatchResponse, LexiconServiceError> {
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
            let current = entry_from_record(record)?;
            ensure_revision(&current, target.base_revision)?;
            let already_target = match target_state {
                TargetState::Archived => current.archived_at.is_some(),
                TargetState::Active => current.archived_at.is_none(),
            };
            if !already_target {
                ensure_lifecycle_revision(&current, target.base_lifecycle_revision)?;
            }
            pending.push((target, current, already_target));
        }

        let surface_sets = pending
            .iter()
            .map(|(_, word, _)| {
                crate::lexicon::repository::surface_projection_sources(word)
                    .map_err(surface_projection_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let entry_ids = pending
            .iter()
            .filter(|(_, _, already_target)| {
                target_state == TargetState::Archived || !*already_target
            })
            .map(|(_, word, _)| word.id)
            .collect::<Vec<_>>();
        let publication_sources =
            LexiconRepository::current_publication_surface_sources(&mut transaction, &entry_ids)
                .await
                .map_err(repository_error)?;
        let surface_keys = crate::lexicon::repository::surface_lock_keys(
            surface_sets
                .iter()
                .map(Vec::as_slice)
                .chain(std::iter::once(publication_sources.as_slice())),
        );
        LexiconRepository::lock_surface_policy_writer(&mut transaction)
            .await
            .map_err(repository_error)?;
        LexiconRepository::lock_surface_keys(&mut transaction, &surface_keys)
            .await
            .map_err(repository_error)?;
        let verified_visibility = if target_state == TargetState::Active {
            self.confirm_restore_visibility(
                &mut transaction,
                actor_id,
                scope,
                &pending,
                &publication_sources,
                confirmed_surface_match_token,
            )
            .await?
        } else {
            None
        };

        let mut words_by_id = HashMap::new();
        let mut affected = 0;
        for (_target, current, already_target) in pending {
            if already_target {
                words_by_id.insert(current.id, current);
                continue;
            }
            if target_state == TargetState::Archived {
                let references = LexiconRepository::active_inbound_sense_refs(
                    &mut transaction,
                    current.id,
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
                let references = LexiconRepository::unavailable_outbound_sense_refs_for_restore(
                    &mut transaction,
                    current.id,
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
            let next = lifecycle_word(current, target_state, actor_id);
            LexiconRepository::transition_lifecycle(&mut transaction, &next, actor_id, request_id)
                .await
                .map_err(repository_error)?;
            affected += 1;
            words_by_id.insert(next.id, next);
        }
        let response = EntryLifecycleBatchResponse {
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
            let audit_word = response.words.first().ok_or_else(invariant_record)?;
            LexiconRepository::insert_command_surface_confirmation_audits(
                &mut transaction,
                actor_id,
                request_id,
                audit_word.id,
                audit_word.revision,
                confirmation,
            )
            .await
            .map_err(repository_error)?;
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

    async fn confirm_restore_visibility(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        actor_id: Uuid,
        scope: &'static str,
        pending: &[(EntryLifecycleTarget, AdminWordV2, bool)],
        publication_sources: &[crate::lexicon::repository::SurfaceProjectionSource],
        token: Option<&str>,
    ) -> Result<Option<VerifiedSurfaceConfirmation>, LexiconServiceError> {
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
        if publication_sources.is_empty() {
            return Ok(None);
        }
        let selection = pending
            .iter()
            .map(|(target, _, _)| {
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
        let items = items.into_values().collect::<Vec<_>>();
        let contexts = contexts.into_values().collect::<Vec<_>>();
        if items.is_empty() {
            return Ok(None);
        }
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
        let command = if scope == RESTORE_SCOPE {
            SurfaceConsumptionCommand::RestoreEntry
        } else {
            SurfaceConsumptionCommand::RestoreEntriesBatch
        };
        let owner_bundle = serde_json::json!({
            "command": if scope == RESTORE_SCOPE { "restore_entry" } else { "restore_entries_batch" },
            "selection": selection,
            "transitions": transitions,
            "match_ids": items.iter().map(|item| &item.match_id).collect::<Vec<_>>(),
            "confirmation_reasons": confirmation_reasons,
        });
        let owner_digest =
            surface_owner_bundle_digest(&owner_bundle).map_err(serialization_error)?;
        let binding = SurfaceConfirmationBinding {
            actor_id,
            command,
            owner_context: serde_json::to_string(&selection).map_err(serialization_error)?,
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
        if !policy.enabled && visibility_required {
            let snapshot = self
                .surface_snapshots
                .create(create_snapshot())
                .await
                .map_err(LexiconServiceError::SurfaceSnapshot)?;
            return Err(
                LexiconServiceError::MultipleActiveExactHeadwordPublicationsNotEnabled(Box::new(
                    snapshot.page,
                )),
            );
        }
        let Some(token) = token else {
            let snapshot = self
                .surface_snapshots
                .create(create_snapshot())
                .await
                .map_err(LexiconServiceError::SurfaceSnapshot)?;
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
                return Err(LexiconServiceError::SurfaceMatchesChanged(Box::new(
                    snapshot.page,
                )));
            }
            Err(error) => return Err(LexiconServiceError::SurfaceSnapshot(error)),
        };
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
        if current_ids != verified_ids || current_digest != verified.match_digest {
            let snapshot = self
                .surface_snapshots
                .create(create_snapshot())
                .await
                .map_err(LexiconServiceError::SurfaceSnapshot)?;
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
    response: EntryLifecycleBatchResponse,
) -> Result<AdminWordV2Envelope, LexiconServiceError> {
    Ok(AdminWordV2Envelope {
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

fn lifecycle_word(mut word: AdminWordV2, target_state: TargetState, actor_id: Uuid) -> AdminWordV2 {
    let now = Utc::now();
    word.lifecycle_revision += 1;
    word.updated_at = now;
    match target_state {
        TargetState::Archived => {
            word.status = AdminWordStatus::Archived;
            word.archived_at = Some(now);
            word.archived_by = Some(actor_id);
        }
        TargetState::Active => {
            word.status = if word.published_revision.is_some() {
                AdminWordStatus::Published
            } else {
                AdminWordStatus::Draft
            };
            word.archived_at = None;
            word.archived_by = None;
        }
    }
    word
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
