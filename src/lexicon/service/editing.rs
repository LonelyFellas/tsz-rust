use super::*;

// --- editor ---

impl LexiconService {
    pub async fn preview_forms_impact(
        &self,
        actor_id: Uuid,
        entry_id: Uuid,
        input: PreviewFormsImpactInputV2,
    ) -> Result<FormsImpactResponseV2, LexiconServiceError> {
        if input.base_revision < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_revision",
                message: "base_revision must be at least 1",
            });
        }
        let current = self.get(entry_id).await?.word;
        ensure_active(&current)?;
        if current.revision != input.base_revision {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: current.revision,
            });
        }
        let storage_issues =
            validate_persisted_text(entry_id, PersistedWordStep::Forms, &input.content);
        if !storage_issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(storage_issues));
        }
        let catalog = self.catalog_context(&input.content).await?;
        let issues = validate_forms(
            entry_id,
            &input.content,
            &current.headwords,
            &catalog.part_codes,
        );
        let blocking = issues
            .into_iter()
            .filter(form_issue_blocks_save)
            .collect::<Vec<_>>();
        if !blocking.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(blocking));
        }
        let next_meanings = reconcile_meanings_after_forms(
            current.meanings.clone(),
            &current.headwords,
            &current.forms,
            &input.content,
            entry_id,
        );
        let proposed = proposed_nodes(&input.content, &next_meanings);
        let limit_issues = validate_node_limit(entry_id, PersistedWordStep::Forms, &proposed);
        if !limit_issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(limit_issues));
        }
        let affected = forms_impact(&current, &input.content, &next_meanings);
        let proposed_word = AdminWordV2 {
            forms: input.content.clone(),
            meanings: next_meanings,
            ..current.clone()
        };
        let (matches, contexts) = self.form_surface_matches(&proposed_word).await?;
        let forms_content_digest = canonical_forms_digest(&input.content)?;
        let policy = if matches.is_empty() {
            None
        } else {
            Some(
                self.surface_policies
                    .policy(SurfacePolicyNameV2::SurfaceWarningAcknowledgement)
                    .await
                    .map_err(LexiconServiceError::SurfacePolicy)?,
            )
        };
        let existing_evidence = if matches.is_empty() {
            None
        } else {
            self.repository
                .forms_surface_acknowledgement_by_entry(entry_id)
                .await
                .map_err(repository_error)?
        };
        let unacknowledged = unacknowledged_forms_matches(
            &matches,
            existing_evidence.as_ref(),
            entry_id,
            &forms_content_digest,
            policy.as_ref(),
        );
        if !unacknowledged.is_empty() {
            let snapshot = self
                .create_forms_surface_snapshot(
                    actor_id,
                    entry_id,
                    current.revision,
                    &input.content,
                    &affected,
                    unacknowledged,
                    contexts,
                    policy.expect("non-empty forms matches have a policy"),
                )
                .await?;
            return Ok(FormsImpactResponseV2 {
                base_revision: current.revision,
                requires_confirmation: !affected.is_empty(),
                affected,
                confirmation_token: None,
                surface_match_page: Some(snapshot.page),
            });
        }
        if affected.is_empty() {
            return Ok(FormsImpactResponseV2 {
                base_revision: current.revision,
                requires_confirmation: false,
                affected,
                confirmation_token: None,
                surface_match_page: None,
            });
        }
        let token = Uuid::now_v7();
        self.impacts
            .save(
                actor_id,
                token,
                &ImpactConfirmation {
                    entry_id,
                    base_revision: current.revision,
                    content_hash: sha256_json(&input.content).map_err(serialization_error)?,
                },
                IMPACT_TTL,
            )
            .await
            .map_err(LexiconServiceError::ImpactStore)?;
        Ok(FormsImpactResponseV2 {
            base_revision: current.revision,
            requires_confirmation: true,
            affected,
            confirmation_token: Some(token),
            surface_match_page: None,
        })
    }

    pub async fn save_forms(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        input: SaveFormsStepInput,
    ) -> Result<AdminWordV2Envelope, LexiconServiceError> {
        if input.base_revision < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_revision",
                message: "base_revision must be at least 1",
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
        ensure_active(&current)?;
        ensure_revision(&current, input.base_revision)?;

        let storage_issues =
            validate_persisted_text(entry_id, PersistedWordStep::Forms, &input.content);
        if !storage_issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(storage_issues));
        }

        let catalog = self
            .catalog_context_for_reference(&mut transaction, &input.content)
            .await?;
        let form_issues = validate_forms(
            entry_id,
            &input.content,
            &current.headwords,
            &catalog.part_codes,
        );
        let blocking = form_issues
            .iter()
            .filter(|issue| form_issue_blocks_save(issue))
            .cloned()
            .collect::<Vec<_>>();
        if !blocking.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(blocking));
        }
        if input.intent == StepSaveIntent::Complete && !form_issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(form_issues));
        }

        let mut meanings = reconcile_meanings_after_forms(
            current.meanings.clone(),
            &current.headwords,
            &current.forms,
            &input.content,
            entry_id,
        );
        // 保证 projection 中 meanings 顺序跟 forms 的 POS 顺序一致。
        meanings.pos.sort_by_key(|meanings_pos| {
            input
                .content
                .pos
                .iter()
                .position(|forms_pos| forms_pos.pos_id == meanings_pos.pos_id)
                .unwrap_or(usize::MAX)
        });
        let reference_resolution = resolve_meaning_references(
            &mut transaction,
            entry_id,
            &mut meanings,
            ReferenceResolutionMode::Canonicalize,
            false,
        )
        .await?;
        if !reference_resolution.issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(
                reference_resolution.issues,
            ));
        }
        let proposed = proposed_nodes(&input.content, &meanings);
        let limit_issues = validate_node_limit(entry_id, PersistedWordStep::Forms, &proposed);
        if !limit_issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(limit_issues));
        }
        let proposed_ids = proposed.iter().map(|node| node.id).collect::<Vec<_>>();
        LexiconRepository::lock_node_ids(&mut transaction, &proposed_ids)
            .await
            .map_err(repository_error)?;
        let existing =
            LexiconRepository::node_identities(&mut transaction, entry_id, &proposed_ids)
                .await
                .map_err(repository_error)?;
        let node_issues = validate_node_identities(entry_id, &input.content, &proposed, &existing);
        if !node_issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(node_issues));
        }

        let affected = forms_impact(&current, &input.content, &meanings);
        let forms_content_digest = canonical_forms_digest(&input.content)?;
        let confirmed_impact_token = input.confirmed_impact_token;
        let confirmed_surface_match_token = input.confirmed_surface_match_token.clone();

        let meaning_issues = validate_meanings(
            entry_id,
            &input.content,
            &meanings,
            &current.headwords,
            &catalog.sub_part_parents,
        );
        let forms_complete = form_issues.is_empty()
            && (input.intent == StepSaveIntent::Complete
                || current.completed_steps.contains(&PersistedWordStep::Forms));
        let meanings_complete = forms_complete
            && meaning_issues.is_empty()
            && current
                .completed_steps
                .contains(&PersistedWordStep::Meanings);
        let completed_steps = completed_steps(forms_complete, meanings_complete);
        let now = Utc::now();
        let previous_forms = current.forms.clone();
        let word = AdminWordV2 {
            revision: current.revision + 1,
            has_unpublished_changes: current.published_revision.is_some(),
            forms: input.content,
            meanings,
            completed_steps,
            max_reachable_step: if meanings_complete {
                WordCreationStep::Preview
            } else if forms_complete {
                WordCreationStep::Meanings
            } else {
                WordCreationStep::Forms
            },
            updated_at: now,
            ..current
        };
        let previous_surface_sources = crate::lexicon::repository::surface_projection_sources(
            &word_with_previous_forms(&word, &previous_forms),
        )
        .map_err(surface_projection_error)?;
        let surface_sources = crate::lexicon::repository::surface_projection_sources(&word)
            .map_err(surface_projection_error)?;
        let surface_keys = crate::lexicon::repository::surface_lock_keys([
            previous_surface_sources.as_slice(),
            surface_sources.as_slice(),
        ]);
        LexiconRepository::lock_surface_policy_writer(&mut transaction)
            .await
            .map_err(repository_error)?;
        LexiconRepository::lock_surface_keys(&mut transaction, &surface_keys)
            .await
            .map_err(repository_error)?;

        // Re-read the authoritative projection only after the same ordered locks
        // used by every surface writer are held. Preview responses are advisory;
        // this lock-held set decides whether acknowledgement is still sufficient.
        let (current_matches, current_contexts) = self
            .form_surface_matches_in_transaction(&mut transaction, &word)
            .await?;
        let current_policy =
            if current_matches.is_empty() && confirmed_surface_match_token.is_none() {
                None
            } else {
                Some(
                    self.surface_policies
                        .policy(SurfacePolicyNameV2::SurfaceWarningAcknowledgement)
                        .await
                        .map_err(LexiconServiceError::SurfacePolicy)?,
                )
            };
        let previous_evidence =
            LexiconRepository::forms_surface_acknowledgement(&mut transaction, entry_id)
                .await
                .map_err(repository_error)?;
        let unacknowledged = unacknowledged_forms_matches(
            &current_matches,
            previous_evidence.as_ref(),
            entry_id,
            &forms_content_digest,
            current_policy.as_ref(),
        );
        let mut verified_surface = None;
        if !unacknowledged.is_empty() {
            let policy = current_policy.expect("non-empty forms matches have a policy");
            let Some(token) = confirmed_surface_match_token.as_deref() else {
                let snapshot = self
                    .create_forms_surface_snapshot(
                        actor_id,
                        entry_id,
                        current.revision,
                        &word.forms,
                        &affected,
                        unacknowledged,
                        current_contexts,
                        policy,
                    )
                    .await?;
                return Err(LexiconServiceError::SurfaceMatchAcknowledgementRequired(
                    Box::new(snapshot.page),
                ));
            };
            let (binding, _) = forms_surface_binding(
                actor_id,
                entry_id,
                current.revision,
                &word.forms,
                &affected,
                policy,
            )?;
            let confirmation = match self
                .surface_snapshots
                .verify(
                    token,
                    &ExpectedSurfaceConfirmation {
                        binding,
                        current_policy: policy,
                    },
                )
                .await
            {
                Ok(confirmation) => confirmation,
                Err(SurfaceSnapshotError::Expired) => {
                    return Err(LexiconServiceError::SurfaceMatchSnapshotExpired);
                }
                Err(SurfaceSnapshotError::PolicyChanged(name)) => {
                    let policy = self
                        .surface_policies
                        .policy(name)
                        .await
                        .map_err(LexiconServiceError::SurfacePolicy)?;
                    return Err(LexiconServiceError::SurfacePolicyChanged(policy));
                }
                Err(SurfaceSnapshotError::BindingMismatch) => {
                    let snapshot = self
                        .create_forms_surface_snapshot(
                            actor_id,
                            entry_id,
                            current.revision,
                            &word.forms,
                            &affected,
                            unacknowledged,
                            current_contexts,
                            policy,
                        )
                        .await?;
                    return Err(LexiconServiceError::SurfaceMatchesChanged(Box::new(
                        snapshot.page,
                    )));
                }
                Err(error) => return Err(LexiconServiceError::SurfaceSnapshot(error)),
            };
            let confirmed_ids = confirmation
                .match_ids
                .iter()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>();
            if unacknowledged
                .iter()
                .any(|item| !confirmed_ids.contains(item.match_id.as_str()))
            {
                let snapshot = self
                    .create_forms_surface_snapshot(
                        actor_id,
                        entry_id,
                        current.revision,
                        &word.forms,
                        &affected,
                        unacknowledged,
                        current_contexts,
                        policy,
                    )
                    .await?;
                return Err(LexiconServiceError::SurfaceMatchesChanged(Box::new(
                    snapshot.page,
                )));
            }
            verified_surface = Some(confirmation);
        }

        let mut verified_impact_snapshot = None;
        if !affected.is_empty() {
            let token = confirmed_impact_token.ok_or_else(|| downstream_required(&affected))?;
            if confirmed_surface_match_token.is_some() {
                let policy = current_policy
                    .expect("a submitted forms surface token always loads the current policy");
                let (expected_binding, _) = forms_surface_binding(
                    actor_id,
                    entry_id,
                    current.revision,
                    &word.forms,
                    &affected,
                    policy,
                )?;
                let impact_confirmation = match self
                    .surface_snapshots
                    .verify_impact(
                        token,
                        &ExpectedSurfaceOwner {
                            actor_id,
                            command: SurfaceConsumptionCommand::SaveForms,
                            owner_context: entry_id.to_string(),
                        },
                    )
                    .await
                {
                    Ok(confirmation) => confirmation,
                    Err(SurfaceSnapshotError::Expired) => {
                        return Err(LexiconServiceError::SurfaceMatchSnapshotExpired);
                    }
                    Err(SurfaceSnapshotError::PolicyChanged(name)) => {
                        let policy = self
                            .surface_policies
                            .policy(name)
                            .await
                            .map_err(LexiconServiceError::SurfacePolicy)?;
                        return Err(LexiconServiceError::SurfacePolicyChanged(policy));
                    }
                    Err(SurfaceSnapshotError::BindingMismatch) => {
                        return Err(downstream_required(&affected));
                    }
                    Err(error) => return Err(LexiconServiceError::SurfaceSnapshot(error)),
                };
                if impact_confirmation.binding != expected_binding
                    || verified_surface
                        .as_ref()
                        .is_some_and(|surface_confirmation| {
                            impact_confirmation.snapshot_id != surface_confirmation.snapshot_id
                        })
                {
                    return Err(downstream_required(&affected));
                }
                verified_impact_snapshot = Some(impact_confirmation);
            } else {
                let confirmation = self
                    .impacts
                    .load(actor_id, token)
                    .await
                    .map_err(LexiconServiceError::ImpactStore)?
                    .ok_or_else(|| downstream_required(&affected))?;
                let expected_hash = sha256_json(&word.forms).map_err(serialization_error)?;
                if confirmation.entry_id != entry_id
                    || confirmation.base_revision != current.revision
                    || confirmation.content_hash != expected_hash
                {
                    return Err(downstream_required(&affected));
                }
            }
        }

        let forms_surface_evidence = if current_matches.is_empty() {
            None
        } else {
            let policy = current_policy.expect("non-empty forms matches have a policy");
            let (acknowledged_by_admin_id, acknowledged_at) = match &verified_surface {
                Some(_) => (actor_id, Utc::now()),
                None => {
                    let evidence = previous_evidence.as_ref().ok_or_else(invariant_record)?;
                    (evidence.acknowledged_by_admin_id, evidence.acknowledged_at)
                }
            };
            let mut match_ids = current_matches
                .iter()
                .map(|item| item.match_id.clone())
                .collect::<Vec<_>>();
            match_ids.sort();
            Some(FormsSurfaceAcknowledgementRecord {
                entry_id,
                forms_revision: word.revision,
                forms_content_digest: forms_content_digest.clone(),
                match_ids,
                match_digest: crate::lexicon::surface_snapshot::surface_match_digest(
                    &current_matches,
                    &[SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches],
                )
                .map_err(LexiconServiceError::SurfaceSnapshot)?,
                acknowledged_by_admin_id,
                acknowledged_at,
                policy_name: "surface_warning_acknowledgement".to_owned(),
                policy_epoch: i64::try_from(policy.epoch).map_err(|_| invariant_record())?,
                normalization_version: i32::from(
                    crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION,
                ),
            })
        };
        LexiconRepository::replace_entry_content(
            &mut transaction,
            &word,
            actor_id,
            request_id,
            "forms",
            &catalog.part_ids,
            &catalog.sub_part_ids,
        )
        .await
        .map_err(repository_error)?;
        LexiconRepository::replace_surface_projection(
            &mut transaction,
            word.id,
            word.revision,
            crate::lexicon::repository::SurfaceContentScope::Draft,
            None,
            &previous_surface_sources,
            &surface_sources,
        )
        .await
        .map_err(repository_error)?;
        if let Some(evidence) = &forms_surface_evidence {
            LexiconRepository::upsert_forms_surface_acknowledgement(&mut transaction, evidence)
                .await
                .map_err(repository_error)?;
        } else {
            LexiconRepository::delete_forms_surface_acknowledgement(&mut transaction, entry_id)
                .await
                .map_err(repository_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        if let Some(confirmation) = verified_surface
            .as_ref()
            .or(verified_impact_snapshot.as_ref())
            && let Err(error) = self.surface_snapshots.remove_verified(confirmation).await
        {
            tracing::warn!(%error, snapshot_id = %confirmation.snapshot_id, "saved forms but failed to remove surface confirmation");
        }
        Ok(AdminWordV2Envelope { word })
    }

    pub async fn save_meanings(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        input: SaveMeaningsStepInput,
    ) -> Result<AdminWordV2Envelope, LexiconServiceError> {
        let SaveMeaningsStepInput {
            base_revision,
            intent,
            mut content,
        } = input;
        if base_revision < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_revision",
                message: "base_revision must be at least 1",
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
        ensure_active(&current)?;
        ensure_revision(&current, base_revision)?;
        if !current.completed_steps.contains(&PersistedWordStep::Forms) {
            return Err(LexiconServiceError::StepNotReachable);
        }
        let storage_issues =
            validate_persisted_text(entry_id, PersistedWordStep::Meanings, &content);
        if !storage_issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(storage_issues));
        }
        let catalog = self
            .catalog_context_for_reference(&mut transaction, &current.forms)
            .await?;
        let rich_text_is_safe = canonicalize_meanings(&mut content);
        let reference_resolution = resolve_meaning_references(
            &mut transaction,
            entry_id,
            &mut content,
            ReferenceResolutionMode::Canonicalize,
            false,
        )
        .await?;
        if !reference_resolution.issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(
                reference_resolution.issues,
            ));
        }
        let issues = validate_meanings(
            entry_id,
            &current.forms,
            &content,
            &current.headwords,
            &catalog.sub_part_parents,
        );
        let proposed = proposed_nodes(&current.forms, &content);
        let limit_issues = validate_node_limit(entry_id, PersistedWordStep::Meanings, &proposed);
        if !limit_issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(limit_issues));
        }
        let proposed_ids = proposed.iter().map(|node| node.id).collect::<Vec<_>>();
        LexiconRepository::lock_node_ids(&mut transaction, &proposed_ids)
            .await
            .map_err(repository_error)?;
        let existing =
            LexiconRepository::node_identities(&mut transaction, entry_id, &proposed_ids)
                .await
                .map_err(repository_error)?;
        let node_issues = validate_node_identities(entry_id, &current.forms, &proposed, &existing);
        if !node_issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(node_issues));
        }
        if !rich_text_is_safe
            || !meaning_storage_is_safe(
                entry_id,
                &current.forms,
                &content,
                &catalog.sub_part_parents,
            )
        {
            return Err(LexiconServiceError::ValidationFailed(issues));
        }
        if intent == StepSaveIntent::Complete && !issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(issues));
        }
        let meanings_complete = issues.is_empty()
            && (intent == StepSaveIntent::Complete
                || current
                    .completed_steps
                    .contains(&PersistedWordStep::Meanings));
        let now = Utc::now();
        let word = AdminWordV2 {
            revision: current.revision + 1,
            has_unpublished_changes: current.published_revision.is_some(),
            meanings: content,
            completed_steps: completed_steps(true, meanings_complete),
            max_reachable_step: if meanings_complete {
                WordCreationStep::Preview
            } else {
                WordCreationStep::Meanings
            },
            updated_at: now,
            ..current
        };
        LexiconRepository::replace_entry_content(
            &mut transaction,
            &word,
            actor_id,
            request_id,
            "meanings",
            &catalog.part_ids,
            &catalog.sub_part_ids,
        )
        .await
        .map_err(repository_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(AdminWordV2Envelope { word })
    }
}

#[derive(Debug, Clone)]
struct FormSurfaceCandidate {
    candidate_ref: String,
    candidate_word_id: Uuid,
    candidate_node_id: Uuid,
    surface: String,
    normalized_surface: String,
    dialect: Dialect,
    pos_id: Uuid,
    pos: String,
    form_type: WordFormTypeV2,
    lookup_keys: Vec<crate::lexicon::model::SurfaceLookupKey>,
}

#[derive(serde::Serialize)]
struct FormsSurfaceOwnerBundle<'a> {
    entry_id: Uuid,
    base_revision: i64,
    content: &'a DraftFormsStepContent,
    affected: &'a [FormsImpactItemV2],
}

impl LexiconService {
    async fn form_surface_matches(
        &self,
        word: &AdminWordV2,
    ) -> Result<(Vec<LexiconSurfaceMatchV2>, Vec<MatchedEntryContextV2>), LexiconServiceError> {
        let candidates = form_surface_candidates(word)?;
        let requested = form_surface_lookup_keys(&candidates);
        let sources = self
            .repository
            .surface_sources("en", &requested, Some(word.id))
            .await
            .map_err(repository_error)?;
        let matches = form_surface_matches_from_sources(&candidates, &sources)?;
        let contexts = self.surface_match_contexts(&matches).await?;
        Ok((matches, contexts))
    }

    pub(super) async fn form_surface_matches_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        word: &AdminWordV2,
    ) -> Result<(Vec<LexiconSurfaceMatchV2>, Vec<MatchedEntryContextV2>), LexiconServiceError> {
        let candidates = form_surface_candidates(word)?;
        let requested = form_surface_lookup_keys(&candidates);
        let sources = LexiconRepository::surface_sources_in_transaction(
            transaction,
            "en",
            &requested,
            Some(word.id),
        )
        .await
        .map_err(repository_error)?;
        let matches = form_surface_matches_from_sources(&candidates, &sources)?;
        let mut entry_ids = matches
            .iter()
            .map(|item| item.existing.word_id)
            .collect::<Vec<_>>();
        entry_ids.sort_unstable();
        entry_ids.dedup();
        let records =
            LexiconRepository::surface_entry_contexts_in_transaction(transaction, &entry_ids)
                .await
                .map_err(repository_error)?;
        let inbound =
            LexiconRepository::surface_inbound_relations_in_transaction(transaction, &entry_ids)
                .await
                .map_err(repository_error)?;
        Ok((matches, surface_contexts_from_records(records, inbound)?))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "forms snapshot must bind the complete save owner, impact, match, context, and policy evidence"
    )]
    async fn create_forms_surface_snapshot(
        &self,
        actor_id: Uuid,
        entry_id: Uuid,
        base_revision: i64,
        content: &DraftFormsStepContent,
        affected: &[FormsImpactItemV2],
        items: Vec<LexiconSurfaceMatchV2>,
        contexts: Vec<MatchedEntryContextV2>,
        policy: SurfaceCreationPolicy,
    ) -> Result<CreatedSurfaceSnapshot, LexiconServiceError> {
        if policy.name != SurfacePolicyNameV2::SurfaceWarningAcknowledgement || !policy.enabled {
            return Err(invariant_record());
        }
        let (binding, owner_bundle) =
            forms_surface_binding(actor_id, entry_id, base_revision, content, affected, policy)?;
        let input = CreateSurfaceSnapshot {
            binding,
            policy_enabled: policy.enabled,
            policy_block_code: None,
            items,
            matched_entry_contexts: contexts,
            confirmation_reasons: vec![SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches],
            owner_bundle,
            page_size: DEFAULT_SURFACE_PAGE_SIZE,
        };
        let snapshot = if affected.is_empty() {
            self.surface_snapshots.create(input).await
        } else {
            self.surface_snapshots
                .create_with_impact_confirmation(input)
                .await
        };
        let snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            Err(SurfaceSnapshotError::PolicyChanged(name)) => {
                let policy = self
                    .surface_policies
                    .policy(name)
                    .await
                    .map_err(LexiconServiceError::SurfacePolicy)?;
                return Err(LexiconServiceError::SurfacePolicyChanged(policy));
            }
            Err(error) => return Err(LexiconServiceError::SurfaceSnapshot(error)),
        };
        Ok(snapshot)
    }
}

fn form_surface_candidates(
    word: &AdminWordV2,
) -> Result<Vec<FormSurfaceCandidate>, LexiconServiceError> {
    let sources = crate::lexicon::repository::surface_projection_sources(word)
        .map_err(surface_projection_error)?;
    let mut candidates = BTreeMap::<String, FormSurfaceCandidate>::new();
    for source in sources
        .into_iter()
        .filter(|source| source.source_kind == "form")
    {
        let candidate_node_id = source.source_node_id.ok_or_else(invariant_record)?;
        let dialect = parse_dialect(source.dialect).ok_or_else(invariant_record)?;
        let pos_id = source.pos_id.ok_or_else(invariant_record)?;
        let pos = source.pos.clone().ok_or_else(invariant_record)?;
        let form_type =
            WordFormTypeV2::try_from(source.form_type.as_deref().ok_or_else(invariant_record)?)
                .map_err(|()| invariant_record())?;
        let key = crate::lexicon::model::SurfaceLookupKey {
            dialect_scope: source.dialect_scope.to_owned(),
            normalized_surface: source.normalized_surface.clone(),
        };
        let candidate = candidates
            .entry(source.source_id.clone())
            .or_insert_with(|| FormSurfaceCandidate {
                candidate_ref: source.source_id.clone(),
                candidate_word_id: word.id,
                candidate_node_id,
                surface: source.surface.clone(),
                normalized_surface: source.normalized_surface.clone(),
                dialect,
                pos_id,
                pos,
                form_type,
                lookup_keys: Vec::new(),
            });
        if !candidate.lookup_keys.contains(&key) {
            candidate.lookup_keys.push(key);
        }
    }
    Ok(candidates.into_values().collect())
}

fn form_surface_matches_from_sources(
    candidates: &[FormSurfaceCandidate],
    sources: &[crate::lexicon::model::SurfaceSourceRecord],
) -> Result<Vec<LexiconSurfaceMatchV2>, LexiconServiceError> {
    let mut sources_by_key =
        BTreeMap::<(&str, &str), Vec<&crate::lexicon::model::SurfaceSourceRecord>>::new();
    for source in sources {
        sources_by_key
            .entry((
                source.matched_dialect_scope.as_str(),
                source.normalized_surface.as_str(),
            ))
            .or_default()
            .push(source);
    }
    let mut matches = BTreeMap::new();
    for candidate in candidates {
        for key in &candidate.lookup_keys {
            if let Some(matched_sources) =
                sources_by_key.get(&(key.dialect_scope.as_str(), key.normalized_surface.as_str()))
            {
                for source in matched_sources {
                    let item = form_surface_match(candidate, source)?;
                    matches.entry(item.match_id.clone()).or_insert(item);
                }
            }
        }
    }
    Ok(matches.into_values().collect())
}

fn form_surface_lookup_keys(
    candidates: &[FormSurfaceCandidate],
) -> Vec<crate::lexicon::model::SurfaceLookupKey> {
    candidates
        .iter()
        .flat_map(|candidate| candidate.lookup_keys.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn form_surface_match(
    candidate: &FormSurfaceCandidate,
    source: &crate::lexicon::model::SurfaceSourceRecord,
) -> Result<LexiconSurfaceMatchV2, LexiconServiceError> {
    let (_, existing) = existing_surface_match(source)?;
    let category = match source.source_kind.as_str() {
        "headword" => SurfaceMatchCategoryV2::FormHeadword,
        "form" => SurfaceMatchCategoryV2::FormForm,
        _ => return Err(invariant_record()),
    };
    let candidate_wire = SurfaceMatchCandidateV2::Form {
        candidate_ref: candidate.candidate_ref.clone(),
        candidate_word_id: candidate.candidate_word_id,
        candidate_node_id: candidate.candidate_node_id,
        surface: candidate.surface.clone(),
        normalized_surface: candidate.normalized_surface.clone(),
        dialect: candidate.dialect,
        pos_id: candidate.pos_id,
        pos: candidate.pos.clone(),
        form_type: candidate.form_type,
    };
    let match_id = crate::platform::hash_token(
        &serde_json::to_string(&serde_json::json!({
            "candidate": &candidate_wire,
            "existing": &existing,
            "normalization_version": source.normalization_version,
        }))
        .map_err(serialization_error)?,
    );
    Ok(LexiconSurfaceMatchV2 {
        match_id,
        match_category: category,
        severity: SurfaceMatchSeverityV2::Warning,
        attention_level: SurfaceAttentionLevelV2::Normal,
        can_continue: SurfaceCanContinueTrue,
        confirmation_reasons: vec![SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches],
        candidate: candidate_wire,
        existing,
    })
}

pub(super) fn canonical_forms_digest(
    content: &DraftFormsStepContent,
) -> Result<String, LexiconServiceError> {
    Ok(crate::platform::hash_token(
        &serde_json::to_string(content).map_err(serialization_error)?,
    ))
}

fn forms_surface_binding(
    actor_id: Uuid,
    entry_id: Uuid,
    base_revision: i64,
    content: &DraftFormsStepContent,
    affected: &[FormsImpactItemV2],
    policy: SurfaceCreationPolicy,
) -> Result<(SurfaceConfirmationBinding, serde_json::Value), LexiconServiceError> {
    let owner_bundle = serde_json::to_value(FormsSurfaceOwnerBundle {
        entry_id,
        base_revision,
        content,
        affected,
    })
    .map_err(serialization_error)?;
    Ok((
        SurfaceConfirmationBinding {
            actor_id,
            command: SurfaceConsumptionCommand::SaveForms,
            owner_context: entry_id.to_string(),
            base_revision: Some(base_revision),
            canonical_content_digest: canonical_forms_digest(content)?,
            owner_evidence_digest: surface_owner_bundle_digest(&owner_bundle)
                .map_err(serialization_error)?,
            normalization_version: crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION,
            policy_name: policy.name,
            policy_epoch: policy.epoch,
        },
        owner_bundle,
    ))
}

fn unacknowledged_forms_matches(
    current: &[LexiconSurfaceMatchV2],
    evidence: Option<&FormsSurfaceAcknowledgementRecord>,
    entry_id: Uuid,
    content_digest: &str,
    policy: Option<&SurfaceCreationPolicy>,
) -> Vec<LexiconSurfaceMatchV2> {
    let Some(policy) = policy else {
        return Vec::new();
    };
    let acknowledged = evidence
        .filter(|evidence| {
            evidence.entry_id == entry_id
                && evidence.forms_revision > 0
                && evidence.forms_content_digest == content_digest
                && evidence.policy_name == "surface_warning_acknowledgement"
                && evidence.policy_epoch == i64::try_from(policy.epoch).unwrap_or_default()
                && evidence.normalization_version
                    == i32::from(crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION)
        })
        .map(|evidence| {
            evidence
                .match_ids
                .iter()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    current
        .iter()
        .filter(|item| !acknowledged.contains(item.match_id.as_str()))
        .cloned()
        .collect()
}

fn word_with_previous_forms(
    word: &AdminWordV2,
    previous_forms: &DraftFormsStepContent,
) -> AdminWordV2 {
    AdminWordV2 {
        forms: previous_forms.clone(),
        ..word.clone()
    }
}

// --- editor support ---

pub(super) fn ensure_active(word: &AdminWordV2) -> Result<(), LexiconServiceError> {
    if word.archived_at.is_some() {
        return Err(LexiconServiceError::EntryArchived);
    }
    Ok(())
}

pub(super) fn ensure_revision(
    word: &AdminWordV2,
    base_revision: i64,
) -> Result<(), LexiconServiceError> {
    if word.revision != base_revision {
        return Err(LexiconServiceError::RevisionConflict {
            current_revision: word.revision,
        });
    }
    Ok(())
}

pub(super) fn completed_steps(forms: bool, meanings: bool) -> Vec<PersistedWordStep> {
    let mut steps = vec![PersistedWordStep::Basics];
    if forms {
        steps.push(PersistedWordStep::Forms);
    }
    if forms && meanings {
        steps.push(PersistedWordStep::Meanings);
    }
    steps
}

pub(super) fn form_issue_blocks_save(issue: &DraftValidationIssue) -> bool {
    matches!(
        issue.code.as_str(),
        "duplicate_part_of_speech"
            | "unknown_part_of_speech"
            | "dialect_rules_invalid"
            | "base_form_type_invalid"
            | "form_type_invalid"
            | "duplicate_form_type"
            | "duplicate_dialect_variant"
            | "spelling_not_trimmed"
            | "spelling_too_long"
            | "spelling_not_normalizable"
            | "dict_phonetic_too_long"
            | "actual_pron_too_long"
            | "node_id_reused"
    )
}

pub(super) fn forms_impact(
    word: &AdminWordV2,
    next_forms: &DraftFormsStepContent,
    next_meanings: &DraftMeaningsStepContent,
) -> Vec<FormsImpactItemV2> {
    let next_pos_codes = next_forms
        .pos
        .iter()
        .map(|pos| (pos.pos_id, pos.pos.as_str()))
        .collect::<HashMap<_, _>>();
    let changed_pos_ids = word
        .forms
        .pos
        .iter()
        .filter(|pos| next_pos_codes.get(&pos.pos_id).copied() != Some(pos.pos.as_str()))
        .map(|pos| pos.pos_id)
        .collect::<std::collections::HashSet<_>>();
    let current_nodes = proposed_nodes(&word.forms, &word.meanings);
    let impactful_pos_ids = word
        .meanings
        .pos
        .iter()
        .filter(|pos| {
            changed_pos_ids.contains(&pos.pos_id)
                && pos_meanings_have_content(word.id, pos, &word.meanings)
        })
        .map(|pos| pos.pos_id)
        .collect::<std::collections::HashSet<_>>();
    let current_by_id = current_nodes
        .iter()
        .map(|node| (node.id, node))
        .collect::<HashMap<_, _>>();
    let next_nodes = proposed_nodes(next_forms, next_meanings);
    let next_by_id = next_nodes
        .iter()
        .map(|node| (node.id, node))
        .collect::<HashMap<_, _>>();
    let mut affected = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for node in &current_nodes {
        if node.node_type != "pos" && node.step != PersistedWordStep::Meanings {
            continue;
        }
        let owning_pos_id = if node.node_type == "pos" {
            Some(node.id)
        } else {
            owning_pos_id(node, &current_by_id)
        };
        if owning_pos_id.is_some_and(|pos_id| {
            changed_pos_ids.contains(&pos_id) && !impactful_pos_ids.contains(&pos_id)
        }) {
            continue;
        }
        let binding_is_unchanged = next_by_id.get(&node.id).is_some_and(|next| *next == node);
        if binding_is_unchanged && !changed_pos_ids.contains(&node.id) {
            continue;
        }
        if !seen.insert(node.id) {
            continue;
        }
        affected.push(FormsImpactItemV2 {
            node_id: node.id,
            node_type: FormsImpactNodeType::from_internal(node.node_type),
            reason: if node.node_type == "pos" && changed_pos_ids.contains(&node.id) {
                "词性被删除或代码被替换，其下游词义内容将重建".to_owned()
            } else {
                "节点将因词形结构变更从草稿中移除或重新绑定".to_owned()
            },
        });
    }
    affected
}

fn owning_pos_id(node: &ProposedNode, nodes: &HashMap<Uuid, &ProposedNode>) -> Option<Uuid> {
    let mut parent_id = node.parent_node_id;
    while let Some(id) = parent_id {
        let parent = nodes.get(&id)?;
        if parent.node_type == "pos" {
            return Some(parent.id);
        }
        parent_id = parent.parent_node_id;
    }
    None
}

fn pos_meanings_have_content(
    entry_id: Uuid,
    pos: &WordPosMeaningsV2,
    meanings: &DraftMeaningsStepContent,
) -> bool {
    if pos.grammar_structures.len() != 1
        || pos.grammar_structures.iter().any(|grammar| {
            grammar
                .variants
                .iter()
                .any(|variant| !variant.content.text().trim().is_empty())
        })
        || pos.senses.len() != 1
    {
        return true;
    }
    let sense = &pos.senses[0];
    if !sense.sub_pos.is_empty()
        || sense.level != "A1"
        || sense.frequency.is_some()
        || sense.depends_on_context
        || !sense.relations.is_empty()
        || sense.definitions.len() != 1
        || sense.sentences.len() != 1
    {
        return true;
    }
    let default_group_id = meanings.sense_groups.first().map(|group| group.id);
    if sense.sense_group_id != default_group_id {
        return true;
    }
    let default_definition = matches!(
        &sense.definitions[0],
        WordDefinitionV2::ZhDefinition {
            level,
            grammar_structure_id: None,
            content,
            ..
        } if level == "A1" && content.text().trim().is_empty()
    );
    if !default_definition {
        return true;
    }
    let sentence = &sense.sentences[0];
    sentence.level != "A1"
        || english_text_has_content(&sentence.en_text)
        || !sentence.zh_text.text().trim().is_empty()
        || sentence.links.len() != 1
        || sentence.links[0].word_id != entry_id
        || sentence.links[0].sense_id != sense.id
        || sentence.links[0].role != "focus"
}

fn english_text_has_content(content: &EnglishTextV2) -> bool {
    match content {
        EnglishTextV2::Unified { common } => !common.value.text().trim().is_empty(),
        EnglishTextV2::Distinguish { uk, us, .. } => [uk, us].iter().any(|slot| {
            matches!(slot, DialectVariantSlotV2::Ready { variant } if !variant.value.text().trim().is_empty())
        }),
    }
}

pub(super) fn downstream_required(affected: &[FormsImpactItemV2]) -> LexiconServiceError {
    LexiconServiceError::DownstreamConfirmationRequired(
        affected.iter().map(|item| item.node_id).collect(),
    )
}

pub(super) fn reconcile_meanings_after_forms(
    mut meanings: DraftMeaningsStepContent,
    headwords: &WordHeadwordsV2,
    previous_forms: &DraftFormsStepContent,
    forms: &DraftFormsStepContent,
    entry_id: Uuid,
) -> DraftMeaningsStepContent {
    if meanings.sense_groups.is_empty() {
        meanings.sense_groups.push(SenseGroupV2 {
            id: Uuid::now_v7(),
            name_zh: String::new(),
            name_en: String::new(),
        });
    }
    let default_group_id = meanings.sense_groups[0].id;
    let previous_codes = previous_forms
        .pos
        .iter()
        .map(|pos| (pos.pos_id, pos.pos.as_str()))
        .collect::<HashMap<_, _>>();
    let unchanged = forms
        .pos
        .iter()
        .filter(|pos| previous_codes.get(&pos.pos_id).copied() == Some(pos.pos.as_str()))
        .map(|pos| pos.pos_id)
        .collect::<std::collections::HashSet<_>>();
    meanings.pos.retain(|pos| unchanged.contains(&pos.pos_id));
    for forms_pos in &forms.pos {
        if !meanings
            .pos
            .iter()
            .any(|pos| pos.pos_id == forms_pos.pos_id)
        {
            meanings.pos.push(build_initial_pos_meanings(
                entry_id,
                headwords,
                forms_pos,
                default_group_id,
            ));
        }
    }
    meanings
}

// --- storage validation ---

pub(super) fn meaning_storage_is_safe(
    entry_id: Uuid,
    forms: &DraftFormsStepContent,
    meanings: &DraftMeaningsStepContent,
    sub_part_parents: &HashMap<String, String>,
) -> bool {
    let pos_codes = forms
        .pos
        .iter()
        .map(|pos| (pos.pos_id, pos.pos.as_str()))
        .collect::<HashMap<_, _>>();
    let group_ids = meanings
        .sense_groups
        .iter()
        .map(|group| group.id)
        .collect::<std::collections::HashSet<_>>();
    let mut seen_pos = std::collections::HashSet::new();
    let mut node_ids = std::collections::HashSet::new();
    for group in &meanings.sense_groups {
        if !node_ids.insert(group.id)
            || group.name_zh.chars().count() > 200
            || group.name_en.chars().count() > 200
        {
            return false;
        }
    }
    for pos in &meanings.pos {
        let Some(pos_code) = pos_codes.get(&pos.pos_id).copied() else {
            return false;
        };
        if !seen_pos.insert(pos.pos_id) {
            return false;
        }
        let grammar_ids = pos
            .grammar_structures
            .iter()
            .map(|grammar| grammar.id)
            .collect::<std::collections::HashSet<_>>();
        for grammar in &pos.grammar_structures {
            if !node_ids.insert(grammar.id) {
                return false;
            }
            let mut dialects = std::collections::HashSet::new();
            for variant in &grammar.variants {
                if !node_ids.insert(variant.id) || !dialects.insert(variant.dialect) {
                    return false;
                }
            }
        }
        for sense in &pos.senses {
            if !node_ids.insert(sense.id)
                || !valid_level(&sense.level)
                || sense
                    .frequency
                    .as_deref()
                    .is_some_and(|value| !valid_fixed_percent(value))
                || sense
                    .sense_group_id
                    .is_some_and(|group_id| !group_ids.contains(&group_id))
                || (!sense.sub_pos.is_empty()
                    && sub_part_parents
                        .get(&sense.sub_pos)
                        .is_none_or(|parent| parent != pos_code))
            {
                return false;
            }
            for definition in &sense.definitions {
                let id = definition_id(definition);
                let grammar_id = definition_grammar_id(definition);
                if !node_ids.insert(id)
                    || !valid_level(definition_level(definition))
                    || grammar_id.is_some_and(|id| !grammar_ids.contains(&id))
                {
                    return false;
                }
            }
            for sentence in &sense.sentences {
                let mut link_targets = std::collections::HashSet::new();
                let focus = sentence
                    .links
                    .iter()
                    .filter(|link| link.role == "focus")
                    .collect::<Vec<_>>();
                if !node_ids.insert(sentence.id)
                    || !valid_level(&sentence.level)
                    || focus.len() > 1
                    || focus
                        .first()
                        .is_some_and(|link| link.word_id != entry_id || link.sense_id != sense.id)
                    || sentence.links.iter().any(|link| {
                        !matches!(link.role.as_str(), "focus" | "context")
                            || !link_targets.insert((link.word_id, link.sense_id))
                    })
                {
                    return false;
                }
            }
            for relation in &sense.relations {
                if !node_ids.insert(relation.id)
                    || !matches!(
                        relation.relation.as_str(),
                        "synonym" | "antonym" | "derivative"
                    )
                    || !valid_fixed_percent(&relation.score)
                {
                    return false;
                }
            }
        }
    }
    true
}

pub(super) fn definition_id(definition: &WordDefinitionV2) -> Uuid {
    match definition {
        WordDefinitionV2::ZhDefinition { id, .. }
        | WordDefinitionV2::ZhSentence { id, .. }
        | WordDefinitionV2::EnDefinition { id, .. }
        | WordDefinitionV2::EnSentence { id, .. } => *id,
    }
}

pub(super) fn definition_grammar_id(definition: &WordDefinitionV2) -> Option<Uuid> {
    match definition {
        WordDefinitionV2::ZhDefinition {
            grammar_structure_id,
            ..
        }
        | WordDefinitionV2::ZhSentence {
            grammar_structure_id,
            ..
        }
        | WordDefinitionV2::EnDefinition {
            grammar_structure_id,
            ..
        }
        | WordDefinitionV2::EnSentence {
            grammar_structure_id,
            ..
        } => *grammar_structure_id,
    }
}

pub(super) fn definition_level(definition: &WordDefinitionV2) -> &str {
    match definition {
        WordDefinitionV2::ZhDefinition { level, .. }
        | WordDefinitionV2::ZhSentence { level, .. }
        | WordDefinitionV2::EnDefinition { level, .. }
        | WordDefinitionV2::EnSentence { level, .. } => level,
    }
}

pub(super) fn valid_fixed_percent(value: &str) -> bool {
    let mut parts = value.split('.');
    let whole = parts.next().unwrap_or_default();
    let decimal = parts.next();
    if parts.next().is_some()
        || whole.is_empty()
        || !whole.chars().all(|character| character.is_ascii_digit())
        || decimal.is_some_and(|value| {
            value.is_empty()
                || value.len() > 2
                || !value.chars().all(|character| character.is_ascii_digit())
        })
    {
        return false;
    }
    value
        .parse::<f64>()
        .is_ok_and(|number| (0.0..=100.0).contains(&number))
}

#[cfg(test)]
mod forms_surface_tests {
    use super::*;

    fn validation_issue(code: &str) -> DraftValidationIssue {
        DraftValidationIssue {
            step: PersistedWordStep::Forms,
            node_id: Uuid::now_v7(),
            field: "field".to_owned(),
            code: code.to_owned(),
            message: "message".to_owned(),
            reference_location: None,
            node_location: None,
        }
    }

    #[test]
    fn draft_save_only_blocks_storage_safety_issues() {
        for code in [
            "dialect_variants_invalid",
            "invalid_form_type_for_part_of_speech",
            "base_spelling_mismatch",
        ] {
            assert!(!form_issue_blocks_save(&validation_issue(code)), "{code}");
        }
        for code in [
            "duplicate_part_of_speech",
            "unknown_part_of_speech",
            "dialect_rules_invalid",
            "base_form_type_invalid",
            "form_type_invalid",
            "duplicate_form_type",
            "duplicate_dialect_variant",
            "spelling_not_normalizable",
            "node_id_reused",
        ] {
            assert!(form_issue_blocks_save(&validation_issue(code)), "{code}");
        }
    }

    fn candidate(entry_id: Uuid, node_id: Uuid) -> FormSurfaceCandidate {
        FormSurfaceCandidate {
            candidate_ref: format!("entry:{entry_id}:form:{node_id}"),
            candidate_word_id: entry_id,
            candidate_node_id: node_id,
            surface: "workspaces".to_owned(),
            normalized_surface: "workspaces".to_owned(),
            dialect: Dialect::Common,
            pos_id: Uuid::now_v7(),
            pos: "noun".to_owned(),
            form_type: WordFormTypeV2::Plural,
            lookup_keys: vec![],
        }
    }

    fn existing_source(source_kind: &str) -> crate::lexicon::model::SurfaceSourceRecord {
        crate::lexicon::model::SurfaceSourceRecord {
            matched_dialect_scope: "uk".to_owned(),
            entry_id: Uuid::now_v7(),
            entry_headword: "workspaces".to_owned(),
            entry_headword_dialect: "common".to_owned(),
            entry_kind: "word".to_owned(),
            lifecycle_status: "draft".to_owned(),
            source_id: format!("existing:{source_kind}"),
            source_kind: source_kind.to_owned(),
            source_node_id: (source_kind == "form").then(Uuid::now_v7),
            content_scope: "draft".to_owned(),
            publication_id: None,
            surface: "workspaces".to_owned(),
            normalized_surface: "workspaces".to_owned(),
            dialect: "common".to_owned(),
            normalization_version: crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION,
            source_revision: 1,
            event_offset: 1,
            pos_id: (source_kind == "form").then(Uuid::now_v7),
            pos: (source_kind == "form").then(|| "noun".to_owned()),
            form_type: (source_kind == "form").then(|| "plural".to_owned()),
        }
    }

    #[test]
    fn form_matches_classify_both_directions_and_preserve_slot_identity() {
        let entry_id = Uuid::now_v7();
        let node_id = Uuid::now_v7();
        let candidate = candidate(entry_id, node_id);
        let headword = form_surface_match(&candidate, &existing_source("headword")).unwrap();
        let form = form_surface_match(&candidate, &existing_source("form")).unwrap();

        assert_eq!(
            headword.match_category,
            SurfaceMatchCategoryV2::FormHeadword
        );
        assert_eq!(form.match_category, SurfaceMatchCategoryV2::FormForm);
        for item in [headword, form] {
            assert_eq!(item.severity, SurfaceMatchSeverityV2::Warning);
            assert_eq!(item.attention_level, SurfaceAttentionLevelV2::Normal);
            assert!(matches!(
                item.candidate,
                SurfaceMatchCandidateV2::Form {
                    candidate_word_id,
                    candidate_node_id,
                    form_type: WordFormTypeV2::Plural,
                    ..
                } if candidate_word_id == entry_id && candidate_node_id == node_id
            ));
        }
    }

    #[test]
    fn form_lookup_keys_are_deduplicated_across_legal_same_surface_slots() {
        let lookup_key = crate::lexicon::model::SurfaceLookupKey {
            dialect_scope: "uk".to_owned(),
            normalized_surface: "workspaces".to_owned(),
        };
        let mut first = candidate(Uuid::now_v7(), Uuid::now_v7());
        first.lookup_keys = vec![lookup_key.clone()];
        let mut second = candidate(Uuid::now_v7(), Uuid::now_v7());
        second.lookup_keys = vec![lookup_key.clone()];

        assert_eq!(form_surface_lookup_keys(&[first, second]), vec![lookup_key]);
    }

    #[test]
    fn forms_evidence_only_suppresses_same_content_policy_and_match_membership() {
        let entry_id = Uuid::now_v7();
        let item = form_surface_match(
            &candidate(entry_id, Uuid::now_v7()),
            &existing_source("headword"),
        )
        .unwrap();
        let evidence = FormsSurfaceAcknowledgementRecord {
            entry_id,
            forms_revision: 2,
            forms_content_digest: "same-content".to_owned(),
            match_ids: vec![item.match_id.clone()],
            match_digest: "digest".to_owned(),
            acknowledged_by_admin_id: Uuid::now_v7(),
            acknowledged_at: Utc::now(),
            policy_name: "surface_warning_acknowledgement".to_owned(),
            policy_epoch: 1,
            normalization_version: i32::from(
                crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION,
            ),
        };
        let policy = SurfaceCreationPolicy {
            enabled: true,
            name: SurfacePolicyNameV2::SurfaceWarningAcknowledgement,
            epoch: 1,
        };

        assert!(
            unacknowledged_forms_matches(
                std::slice::from_ref(&item),
                Some(&evidence),
                entry_id,
                "same-content",
                Some(&policy),
            )
            .is_empty()
        );
        assert_eq!(
            unacknowledged_forms_matches(
                std::slice::from_ref(&item),
                Some(&evidence),
                entry_id,
                "changed-content",
                Some(&policy),
            )
            .len(),
            1
        );
        assert_eq!(
            unacknowledged_forms_matches(
                &[form_surface_match(
                    &candidate(entry_id, Uuid::now_v7()),
                    &existing_source("headword"),
                )
                .unwrap()],
                Some(&evidence),
                entry_id,
                "same-content",
                Some(&policy),
            )
            .len(),
            1
        );
    }
}
