use super::*;

// --- publication ---

impl LexiconService {
    pub async fn validate(
        &self,
        entry_id: Uuid,
        input: ValidateAdminWordV2Input,
    ) -> Result<DraftValidationResponse, LexiconServiceError> {
        let word = self.get(entry_id).await?.word;
        ensure_active(&word)?;
        ensure_revision(&word, input.base_revision)?;
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let catalog = self
            .catalog_context_for_reference(&mut transaction, &word.forms)
            .await?;
        let mut issues =
            validate_forms(entry_id, &word.forms, &word.headwords, &catalog.part_codes);
        issues.extend(validate_meanings(
            entry_id,
            &word.forms,
            &word.meanings,
            &word.headwords,
            &catalog.sub_part_parents,
        ));
        let mut meanings = word.meanings.clone();
        let reference_resolution = resolve_meaning_references(
            &mut transaction,
            entry_id,
            &mut meanings,
            ReferenceResolutionMode::Verify,
            false,
        )
        .await?;
        issues.extend(reference_resolution.issues);
        transaction.commit().await.map_err(database_error)?;
        Ok(DraftValidationResponse {
            validated_revision: word.revision,
            valid: issues.is_empty(),
            issues,
        })
    }

    pub async fn publish(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        idempotency_key: Uuid,
        input: PublishAdminWordV2Input,
    ) -> Result<AdminWordV2Envelope, LexiconServiceError> {
        if input.base_revision < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_revision",
                message: "base_revision must be at least 1",
            });
        }
        let request_hash = sha256_json(&serde_json::json!({
            "entry_id": entry_id,
            "input": input,
        }))
        .map_err(serialization_error)?;
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{PUBLISH_SCOPE}:{actor_id}:{idempotency_key}"))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if let Some(existing) = LexiconRepository::idempotency(
            &mut transaction,
            PUBLISH_SCOPE,
            actor_id,
            idempotency_key,
        )
        .await
        .map_err(repository_error)?
        {
            if existing.request_hash != request_hash {
                return Err(LexiconServiceError::IdempotencyConflict);
            }
            transaction.commit().await.map_err(database_error)?;
            return serde_json::from_value(existing.response_body).map_err(serialization_error);
        }

        LexiconRepository::lock_surface_policy_writer(&mut transaction)
            .await
            .map_err(repository_error)?;
        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        let current_publication_id = record.current_publication_id;
        let current_publication_source_revision = record.current_publication_source_revision;
        let current_published_at = record.current_published_at;
        let mut word = entry_from_record(record)?;
        ensure_active(&word)?;
        ensure_revision(&word, input.base_revision)?;
        let surface_sources = crate::lexicon::repository::surface_projection_sources(&word)
            .map_err(surface_projection_error)?;
        let previous_publication_sources =
            LexiconRepository::current_publication_surface_sources(&mut transaction, &[entry_id])
                .await
                .map_err(repository_error)?;
        let surface_keys = crate::lexicon::repository::surface_lock_keys([
            previous_publication_sources.as_slice(),
            surface_sources.as_slice(),
        ]);
        LexiconRepository::lock_surface_keys(&mut transaction, &surface_keys)
            .await
            .map_err(repository_error)?;
        let command_owner = serde_json::json!({
            "entry_id": word.id,
            "base_revision": word.revision,
        });
        let verified_visibility = self
            .confirm_visibility_command(
                &mut transaction,
                actor_id,
                &word,
                &previous_publication_sources,
                &surface_sources,
                input.confirmed_surface_match_token.as_deref(),
                SurfaceConsumptionCommand::PublishEntry,
                "publish_entry",
                command_owner,
            )
            .await?;
        let catalog = self
            .catalog_context_for_reference(&mut transaction, &word.forms)
            .await?;
        let mut issues =
            validate_forms(entry_id, &word.forms, &word.headwords, &catalog.part_codes);
        issues.extend(validate_meanings(
            entry_id,
            &word.forms,
            &word.meanings,
            &word.headwords,
            &catalog.sub_part_parents,
        ));
        if !issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(issues));
        }

        if current_publication_source_revision == Some(word.revision) {
            word.status = AdminWordStatus::Published;
            word.published_revision = current_publication_source_revision;
            word.has_unpublished_changes = false;
            word.published_at = current_published_at;
            if let Some(confirmation) = verified_visibility.as_ref() {
                LexiconRepository::insert_command_surface_confirmation_audits(
                    &mut transaction,
                    actor_id,
                    request_id,
                    word.id,
                    word.revision,
                    confirmation,
                )
                .await
                .map_err(repository_error)?;
            }
            LexiconRepository::insert_idempotent_word_response(
                &mut transaction,
                PUBLISH_SCOPE,
                actor_id,
                idempotency_key,
                &request_hash,
                current_publication_id,
                &word,
                201,
            )
            .await
            .map_err(repository_error)?;
            transaction.commit().await.map_err(database_error)?;
            if let Some(confirmation) = verified_visibility
                && let Err(error) = self.surface_snapshots.remove_verified(&confirmation).await
            {
                tracing::warn!(
                    ?error,
                    "failed to remove consumed publish visibility snapshot"
                );
            }
            return Ok(AdminWordV2Envelope { word });
        }

        let reference_resolution = resolve_meaning_references(
            &mut transaction,
            entry_id,
            &mut word.meanings,
            ReferenceResolutionMode::Verify,
            true,
        )
        .await?;
        if !reference_resolution.issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(
                reference_resolution.issues,
            ));
        }
        let retained_sense_ids = word
            .meanings
            .pos
            .iter()
            .flat_map(|pos| pos.senses.iter().map(|sense| sense.id))
            .collect::<Vec<_>>();
        let inbound_references = LexiconRepository::current_inbound_sense_refs(
            &mut transaction,
            entry_id,
            &retained_sense_ids,
        )
        .await
        .map_err(repository_error)?;
        if !inbound_references.is_empty() {
            let issues = inbound_references
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
                .collect();
            return Err(LexiconServiceError::ValidationFailed(issues));
        }

        // 回滚到历史发布版本只换 current publication，草稿 revision 原地不动，于是
        // current publication 的 source_revision 会落后于 word.revision，派生出
        // has_unpublished_changes=true。但 word.revision 自己早已有对应的 publication
        // ——revision 只随内容保存推进，同一 revision 的草稿内容与快照逐字相同——
        // 再插一条只会撞 (entry_id, source_revision) 唯一约束。这里唯一合理的语义是把
        // 那条 publication 重新设为当前版本，等价于对它做一次 activate。
        if let Some(publication) = LexiconRepository::publication_by_source_revision_for_update(
            &mut transaction,
            entry_id,
            word.revision,
        )
        .await
        .map_err(repository_error)?
        {
            word.status = AdminWordStatus::Published;
            word.published_revision = Some(publication.source_revision);
            word.has_unpublished_changes = false;
            word.published_at = Some(publication.published_at);
            word.lifecycle_revision += 1;
            word.updated_at = Utc::now();
            LexiconRepository::replace_surface_projection(
                &mut transaction,
                word.id,
                word.revision,
                crate::lexicon::repository::SurfaceContentScope::CurrentPublication(publication.id),
                current_publication_id,
                &previous_publication_sources,
                &surface_sources,
            )
            .await
            .map_err(repository_error)?;
            LexiconRepository::activate_historical_publication(
                &mut transaction,
                actor_id,
                request_id,
                PUBLISH_SCOPE,
                201,
                idempotency_key,
                &request_hash,
                &publication,
                current_publication_id,
                &word,
            )
            .await
            .map_err(repository_error)?;
            if let Some(confirmation) = verified_visibility.as_ref() {
                LexiconRepository::insert_command_surface_confirmation_audits(
                    &mut transaction,
                    actor_id,
                    request_id,
                    word.id,
                    word.revision,
                    confirmation,
                )
                .await
                .map_err(repository_error)?;
            }
            transaction.commit().await.map_err(database_error)?;
            if let Some(confirmation) = verified_visibility
                && let Err(error) = self.surface_snapshots.remove_verified(&confirmation).await
            {
                tracing::warn!(
                    ?error,
                    "failed to remove consumed publish visibility snapshot"
                );
            }
            return Ok(AdminWordV2Envelope { word });
        }

        word.status = AdminWordStatus::Published;
        word.published_revision = Some(word.revision);
        word.has_unpublished_changes = false;
        word.published_at = Some(Utc::now());
        let publication_id = LexiconRepository::insert_publication(
            &mut transaction,
            &word,
            actor_id,
            request_id,
            idempotency_key,
            &request_hash,
            &reference_resolution.publication_references,
        )
        .await
        .map_err(repository_error)?;
        LexiconRepository::replace_surface_projection(
            &mut transaction,
            word.id,
            word.revision,
            crate::lexicon::repository::SurfaceContentScope::CurrentPublication(publication_id),
            current_publication_id,
            &previous_publication_sources,
            &surface_sources,
        )
        .await
        .map_err(repository_error)?;
        if let Some(confirmation) = verified_visibility.as_ref() {
            LexiconRepository::insert_command_surface_confirmation_audits(
                &mut transaction,
                actor_id,
                request_id,
                word.id,
                word.revision,
                confirmation,
            )
            .await
            .map_err(repository_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        if let Some(confirmation) = verified_visibility
            && let Err(error) = self.surface_snapshots.remove_verified(&confirmation).await
        {
            tracing::warn!(
                ?error,
                "failed to remove consumed publish visibility snapshot"
            );
        }
        Ok(AdminWordV2Envelope { word })
    }

    pub async fn activate_publication(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        publication_id: Uuid,
        idempotency_key: Uuid,
        input: ActivatePublicationInput,
    ) -> Result<AdminWordV2Envelope, LexiconServiceError> {
        if input.base_revision < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_revision",
                message: "base_revision must be at least 1",
            });
        }
        if input.base_lifecycle_revision < 1 {
            return Err(LexiconServiceError::InvalidField {
                field: "base_lifecycle_revision",
                message: "base_lifecycle_revision must be at least 1",
            });
        }
        let request_hash = sha256_json(&serde_json::json!({
            "entry_id": entry_id,
            "publication_id": publication_id,
            "input": input,
        }))
        .map_err(serialization_error)?;
        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "{ACTIVATE_PUBLICATION_SCOPE}:{actor_id}:{idempotency_key}"
            ))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if let Some(existing) = LexiconRepository::idempotency(
            &mut transaction,
            ACTIVATE_PUBLICATION_SCOPE,
            actor_id,
            idempotency_key,
        )
        .await
        .map_err(repository_error)?
        {
            if existing.request_hash != request_hash {
                return Err(LexiconServiceError::IdempotencyConflict);
            }
            transaction.commit().await.map_err(database_error)?;
            return serde_json::from_value(existing.response_body).map_err(serialization_error);
        }

        LexiconRepository::lock_surface_policy_writer(&mut transaction)
            .await
            .map_err(repository_error)?;
        let record = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        let previous_publication_id = record.current_publication_id;
        let mut word = entry_from_record(record)?;
        ensure_active(&word)?;
        ensure_revision(&word, input.base_revision)?;
        super::lifecycle::ensure_lifecycle_revision(&word, input.base_lifecycle_revision)?;
        let publication = LexiconRepository::historical_publication_for_update(
            &mut transaction,
            entry_id,
            publication_id,
        )
        .await
        .map_err(repository_error)?
        .ok_or(LexiconServiceError::PublicationNotFound)?;

        if previous_publication_id == Some(publication_id) {
            LexiconRepository::insert_idempotent_word_response(
                &mut transaction,
                ACTIVATE_PUBLICATION_SCOPE,
                actor_id,
                idempotency_key,
                &request_hash,
                Some(publication_id),
                &word,
                200,
            )
            .await
            .map_err(repository_error)?;
            transaction.commit().await.map_err(database_error)?;
            return Ok(AdminWordV2Envelope { word });
        }

        let mut publication_word: AdminWordV2 =
            serde_json::from_value(publication.snapshot.clone()).map_err(serialization_error)?;
        if publication.entry_id != entry_id || publication_word.id != entry_id {
            return Err(invariant_record());
        }
        publication_word.status = AdminWordStatus::Published;
        publication_word.archived_at = None;
        publication_word.archived_by = None;
        publication_word.published_revision = Some(publication.source_revision);
        publication_word.published_at = Some(publication.published_at);
        let previous_sources =
            LexiconRepository::current_publication_surface_sources(&mut transaction, &[entry_id])
                .await
                .map_err(repository_error)?;
        let proposed_sources =
            crate::lexicon::repository::surface_projection_sources(&publication_word)
                .map_err(surface_projection_error)?;
        let surface_keys = crate::lexicon::repository::surface_lock_keys([
            previous_sources.as_slice(),
            proposed_sources.as_slice(),
        ]);
        LexiconRepository::lock_surface_keys(&mut transaction, &surface_keys)
            .await
            .map_err(repository_error)?;
        let command_owner = serde_json::json!({
            "entry_id": entry_id,
            "publication_id": publication_id,
            "base_revision": input.base_revision,
            "base_lifecycle_revision": input.base_lifecycle_revision,
        });
        let verified_visibility = self
            .confirm_visibility_command(
                &mut transaction,
                actor_id,
                &publication_word,
                &previous_sources,
                &proposed_sources,
                input.confirmed_surface_match_token.as_deref(),
                SurfaceConsumptionCommand::ActivatePublication,
                "activate_publication",
                command_owner,
            )
            .await?;
        LexiconRepository::lock_outbound_sense_ref_targets_for_publication(
            &mut transaction,
            publication_id,
        )
        .await
        .map_err(repository_error)?;
        let unavailable = LexiconRepository::unavailable_outbound_sense_refs_for_publication(
            &mut transaction,
            publication_id,
        )
        .await
        .map_err(repository_error)?;
        if !unavailable.is_empty() {
            return Err(LexiconServiceError::EntryHasUnavailablePublicationRefs(
                unavailable,
            ));
        }
        let retained_sense_ids = publication_word
            .meanings
            .pos
            .iter()
            .flat_map(|pos| pos.senses.iter().map(|sense| sense.id))
            .collect::<Vec<_>>();
        let inbound_references = LexiconRepository::current_inbound_sense_refs(
            &mut transaction,
            entry_id,
            &retained_sense_ids,
        )
        .await
        .map_err(repository_error)?;
        if !inbound_references.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(
                inbound_references
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
            ));
        }

        word.status = AdminWordStatus::Published;
        word.published_revision = Some(publication.source_revision);
        word.has_unpublished_changes = word.revision != publication.source_revision;
        word.published_at = Some(publication.published_at);
        word.lifecycle_revision += 1;
        word.updated_at = Utc::now();
        LexiconRepository::replace_surface_projection(
            &mut transaction,
            entry_id,
            word.revision,
            crate::lexicon::repository::SurfaceContentScope::CurrentPublication(publication_id),
            previous_publication_id,
            &previous_sources,
            &proposed_sources,
        )
        .await
        .map_err(repository_error)?;
        LexiconRepository::activate_historical_publication(
            &mut transaction,
            actor_id,
            request_id,
            ACTIVATE_PUBLICATION_SCOPE,
            200,
            idempotency_key,
            &request_hash,
            &publication,
            previous_publication_id,
            &word,
        )
        .await
        .map_err(repository_error)?;
        if let Some(confirmation) = verified_visibility.as_ref() {
            LexiconRepository::insert_command_surface_confirmation_audits(
                &mut transaction,
                actor_id,
                request_id,
                word.id,
                word.revision,
                confirmation,
            )
            .await
            .map_err(repository_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        if let Some(confirmation) = verified_visibility
            && let Err(error) = self.surface_snapshots.remove_verified(&confirmation).await
        {
            tracing::warn!(?error, "failed to remove consumed activation snapshot");
        }
        Ok(AdminWordV2Envelope { word })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn confirm_visibility_command(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        actor_id: Uuid,
        word: &AdminWordV2,
        previous_sources: &[crate::lexicon::repository::SurfaceProjectionSource],
        proposed_sources: &[crate::lexicon::repository::SurfaceProjectionSource],
        token: Option<&str>,
        command: SurfaceConsumptionCommand,
        command_name: &'static str,
        command_owner: serde_json::Value,
    ) -> Result<Option<VerifiedSurfaceConfirmation>, LexiconServiceError> {
        let removals = crate::lexicon::visibility::headword_memberships(previous_sources);
        let additions = crate::lexicon::visibility::headword_memberships(proposed_sources);
        let mut requested = removals
            .iter()
            .chain(additions.iter())
            .map(|(scope, _)| scope.clone())
            .collect::<Vec<_>>();
        requested.sort();
        requested.dedup();
        let before =
            LexiconRepository::active_headword_memberships_in_transaction(transaction, &requested)
                .await
                .map_err(repository_error)?;
        let transitions = crate::lexicon::visibility::transitions(before, removals, additions);
        let visibility_required =
            crate::lexicon::visibility::requires_multiple_active_confirmation(&transitions);

        let active_ids = transitions
            .iter()
            .flat_map(|item| item.after_active_ids.iter().copied())
            .filter(|id| *id != word.id)
            .collect::<std::collections::HashSet<_>>();
        let (mut headword_matches, headword_contexts) = self
            .headword_surface_matches_in_transaction(
                transaction,
                &word.headwords,
                word.kind,
                Some(word.id),
            )
            .await?;
        for item in &mut headword_matches {
            if let SurfaceMatchCandidateV2::Headword {
                candidate_word_id, ..
            } = &mut item.candidate
            {
                *candidate_word_id = Some(word.id);
            }
        }
        let (form_matches, form_contexts) = self
            .form_surface_matches_in_transaction(transaction, word)
            .await?;

        let headword_evidence =
            LexiconRepository::headword_surface_acknowledgement(transaction, word.id)
                .await
                .map_err(repository_error)?;
        let forms_evidence = LexiconRepository::forms_surface_acknowledgement(transaction, word.id)
            .await
            .map_err(repository_error)?;
        let acknowledged_headword_ids = self
            .valid_headword_acknowledgement_ids(word, headword_evidence.as_ref())
            .await?;
        let acknowledged_form_ids = self
            .valid_forms_acknowledgement_ids(word, forms_evidence.as_ref())
            .await?;
        let ordinary_items = headword_matches
            .iter()
            .chain(form_matches.iter())
            .filter(|item| {
                let acknowledged = match item.candidate {
                    SurfaceMatchCandidateV2::Headword { .. } => &acknowledged_headword_ids,
                    SurfaceMatchCandidateV2::Form { .. } => &acknowledged_form_ids,
                };
                !acknowledged.contains(item.match_id.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();

        let mut visibility_items = headword_matches.clone();
        visibility_items.retain(|item| {
            active_ids.contains(&item.existing.word_id)
                && matches!(
                    item.existing.source,
                    ExistingSurfaceSourceV2::Headword {
                        content_scope: SurfaceContentScopeV2::CurrentPublication,
                        ..
                    }
                )
        });
        for item in &mut visibility_items {
            item.confirmation_reasons = vec![SurfaceConfirmationReasonV2::VisibilityActivation];
        }
        let mut items_by_id = std::collections::BTreeMap::new();
        for mut item in ordinary_items {
            item.confirmation_reasons =
                vec![SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches];
            items_by_id.insert(item.match_id.clone(), item);
        }
        if visibility_required {
            for item in visibility_items {
                match items_by_id.entry(item.match_id.clone()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
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
        let items = items_by_id.into_values().collect::<Vec<_>>();
        if items.is_empty() {
            return Ok(None);
        }
        let item_entry_ids = items
            .iter()
            .map(|item| item.existing.word_id)
            .collect::<std::collections::HashSet<_>>();
        let contexts = headword_contexts
            .into_iter()
            .chain(form_contexts)
            .filter(|context| item_entry_ids.contains(&context.word_id))
            .fold(
                std::collections::BTreeMap::new(),
                |mut contexts, context| {
                    contexts.entry(context.word_id).or_insert(context);
                    contexts
                },
            )
            .into_values()
            .collect::<Vec<_>>();

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
        let owner_bundle = serde_json::json!({
            "command": command_name,
            "owner": command_owner,
            "transitions": transitions,
            "match_ids": items.iter().map(|item| &item.match_id).collect::<Vec<_>>(),
            "confirmation_reasons": confirmation_reasons,
        });
        let binding = SurfaceConfirmationBinding {
            actor_id,
            command,
            owner_context: serde_json::to_string(&command_owner).map_err(serialization_error)?,
            base_revision: Some(word.revision),
            canonical_content_digest: canonical_headwords_digest(&word.headwords)?,
            owner_evidence_digest: surface_owner_bundle_digest(&owner_bundle)
                .map_err(serialization_error)?,
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

    pub(super) async fn valid_headword_acknowledgement_ids(
        &self,
        word: &AdminWordV2,
        evidence: Option<&HeadwordSurfaceAcknowledgementRecord>,
    ) -> Result<std::collections::HashSet<String>, LexiconServiceError> {
        let Some(evidence) = evidence else {
            return Ok(Default::default());
        };
        let Some(policy_name) = stored_surface_policy_name(&evidence.policy_name) else {
            return Ok(Default::default());
        };
        let policy = self
            .surface_policies
            .policy(policy_name)
            .await
            .map_err(LexiconServiceError::SurfacePolicy)?;
        if evidence.entry_id != word.id
            || evidence.headwords_content_digest != canonical_headwords_digest(&word.headwords)?
            || evidence.policy_epoch != i64::try_from(policy.epoch).unwrap_or_default()
            || evidence.normalization_version
                != i32::from(crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION)
        {
            return Ok(Default::default());
        }
        Ok(evidence.match_ids.iter().cloned().collect())
    }

    pub(super) async fn valid_forms_acknowledgement_ids(
        &self,
        word: &AdminWordV2,
        evidence: Option<&FormsSurfaceAcknowledgementRecord>,
    ) -> Result<std::collections::HashSet<String>, LexiconServiceError> {
        let Some(evidence) = evidence else {
            return Ok(Default::default());
        };
        let Some(policy_name) = stored_surface_policy_name(&evidence.policy_name) else {
            return Ok(Default::default());
        };
        let policy = self
            .surface_policies
            .policy(policy_name)
            .await
            .map_err(LexiconServiceError::SurfacePolicy)?;
        if evidence.entry_id != word.id
            || evidence.forms_content_digest != canonical_forms_digest(&word.forms)?
            || evidence.policy_epoch != i64::try_from(policy.epoch).unwrap_or_default()
            || evidence.normalization_version
                != i32::from(crate::lexicon::normalization::HEADWORD_NORMALIZATION_VERSION)
        {
            return Ok(Default::default());
        }
        Ok(evidence.match_ids.iter().cloned().collect())
    }
}

fn stored_surface_policy_name(value: &str) -> Option<SurfacePolicyNameV2> {
    match value {
        "surface_warning_acknowledgement" => {
            Some(SurfacePolicyNameV2::SurfaceWarningAcknowledgement)
        }
        "allow_new_exact_headword_entries" => {
            Some(SurfacePolicyNameV2::AllowNewExactHeadwordEntries)
        }
        _ => None,
    }
}

// --- references ---

#[derive(Debug, Clone, Copy)]
pub(super) enum ReferenceResolutionMode {
    Canonicalize,
    Verify,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ReferenceUseKind {
    Relation,
    SentenceContext,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ReferenceUse {
    source_node_id: Uuid,
    target: SenseTargetKey,
    kind: ReferenceUseKind,
    external: bool,
}

#[derive(Debug)]
pub(super) struct ResolvedReferenceSnapshot {
    target_publication_id: Uuid,
    headword: String,
    gloss: String,
}

#[derive(Debug, Default)]
pub(super) struct MeaningReferenceResolution {
    pub(super) issues: Vec<DraftValidationIssue>,
    pub(super) publication_references: Vec<NewPublicationSenseReference>,
}

pub(super) async fn resolve_meaning_references(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    entry_id: Uuid,
    meanings: &mut DraftMeaningsStepContent,
    mode: ReferenceResolutionMode,
    lock_for_publish: bool,
) -> Result<MeaningReferenceResolution, LexiconServiceError> {
    let active_sense_ids = meanings
        .pos
        .iter()
        .flat_map(|pos| pos.senses.iter().map(|sense| sense.id))
        .collect::<std::collections::HashSet<_>>();
    let mut uses = Vec::new();
    let mut issues = Vec::new();

    for pos in &meanings.pos {
        for sense in &pos.senses {
            for sentence in &sense.sentences {
                for link in &sentence.links {
                    if link.role != "context" {
                        continue;
                    }
                    if link.word_id == entry_id {
                        if !active_sense_ids.contains(&link.sense_id) {
                            issues.push(reference_issue(
                                sentence.id,
                                "links",
                                "sentence_context_target_unavailable",
                                "例句 context 必须指向当前草稿中的有效词义",
                            ));
                        }
                    } else {
                        uses.push(ReferenceUse {
                            source_node_id: sentence.id,
                            target: SenseTargetKey {
                                target_entry_id: link.word_id,
                                target_sense_id: link.sense_id,
                            },
                            kind: ReferenceUseKind::SentenceContext,
                            external: true,
                        });
                    }
                }
            }
            for relation in &sense.relations {
                if relation.target_word_id == entry_id {
                    issues.push(reference_issue(
                        relation.id,
                        "target_word_id",
                        "relation_self_target",
                        "关联词不能指向当前词条自身",
                    ));
                    continue;
                }
                uses.push(ReferenceUse {
                    source_node_id: relation.id,
                    target: SenseTargetKey {
                        target_entry_id: relation.target_word_id,
                        target_sense_id: relation.target_sense_id,
                    },
                    kind: ReferenceUseKind::Relation,
                    external: true,
                });
            }
        }
    }

    let requested = uses
        .iter()
        .map(|usage| usage.target)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let records = if lock_for_publish {
        LexiconRepository::resolve_current_published_senses_for_publish(tx, &requested).await
    } else {
        LexiconRepository::resolve_current_published_senses(tx, &requested).await
    }
    .map_err(repository_error)?;
    let mut resolved = HashMap::new();
    for record in records {
        let key = SenseTargetKey {
            target_entry_id: record.target_entry_id,
            target_sense_id: record.target_sense_id,
        };
        let (headword, gloss) = published_sense_snapshot(&record)?;
        resolved.insert(
            key,
            ResolvedReferenceSnapshot {
                target_publication_id: record.target_publication_id,
                headword,
                gloss,
            },
        );
    }

    for usage in &uses {
        if resolved.contains_key(&usage.target) {
            continue;
        }
        let (field, code, message) = match usage.kind {
            ReferenceUseKind::Relation => (
                "target_sense_id",
                "relation_target_unavailable",
                "关联词目标必须是目标词条当前发布版本中的有效词义",
            ),
            ReferenceUseKind::SentenceContext => (
                "links",
                "sentence_context_target_unavailable",
                "例句 context 必须是目标词条当前发布版本中的有效词义",
            ),
        };
        issues.push(reference_issue(usage.source_node_id, field, code, message));
    }

    for pos in &mut meanings.pos {
        for sense in &mut pos.senses {
            for relation in &mut sense.relations {
                let key = SenseTargetKey {
                    target_entry_id: relation.target_word_id,
                    target_sense_id: relation.target_sense_id,
                };
                let Some(snapshot) = resolved.get(&key) else {
                    continue;
                };
                match mode {
                    ReferenceResolutionMode::Canonicalize => {
                        relation.target_headword = Some(snapshot.headword.clone());
                        relation.target_gloss = Some(snapshot.gloss.clone());
                    }
                    ReferenceResolutionMode::Verify => {
                        if relation.target_headword.as_deref() != Some(snapshot.headword.as_str())
                            || relation.target_gloss.as_deref() != Some(snapshot.gloss.as_str())
                        {
                            issues.push(reference_issue(
                                relation.id,
                                "target_sense_id",
                                "relation_target_stale",
                                "关联词目标的当前发布内容已变化，请重新保存词义步骤",
                            ));
                        }
                    }
                }
            }
        }
    }

    let mut seen_publication_refs = std::collections::HashSet::new();
    let mut publication_references = Vec::new();
    for usage in uses {
        if !usage.external {
            continue;
        }
        let Some(snapshot) = resolved.get(&usage.target) else {
            continue;
        };
        let reference_kind = match usage.kind {
            ReferenceUseKind::Relation => PublicationSenseReferenceKind::Relation,
            ReferenceUseKind::SentenceContext => PublicationSenseReferenceKind::SentenceContext,
        };
        let dedupe_key = (
            usage.source_node_id,
            reference_kind.as_str(),
            usage.target.target_entry_id,
            usage.target.target_sense_id,
        );
        if seen_publication_refs.insert(dedupe_key) {
            publication_references.push(NewPublicationSenseReference {
                source_node_id: usage.source_node_id,
                reference_kind,
                target_entry_id: usage.target.target_entry_id,
                target_sense_id: usage.target.target_sense_id,
                target_publication_id: snapshot.target_publication_id,
            });
        }
    }

    Ok(MeaningReferenceResolution {
        issues,
        publication_references,
    })
}

pub(super) fn published_sense_snapshot(
    record: &ResolvedSenseTargetRecord,
) -> Result<(String, String), LexiconServiceError> {
    let word: AdminWordV2 =
        serde_json::from_value(record.snapshot.clone()).map_err(serialization_error)?;
    let headword = published_word_headword(&word);
    let sense = word
        .meanings
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .find(|sense| sense.id == record.target_sense_id)
        .ok_or_else(|| {
            LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
                "published sense node is missing from its snapshot",
            ))
        })?;
    Ok((headword, published_sense_gloss(sense)))
}

pub(super) fn published_word_headword(word: &AdminWordV2) -> String {
    ordered_headword_sides(&word.headwords)
        .into_iter()
        .map(|(_, spelling)| spelling)
        .collect::<Vec<_>>()
        .join(" / ")
}

pub(super) fn published_sense_gloss(sense: &WordSenseV2) -> String {
    sense
        .definitions
        .iter()
        .find_map(|definition| match definition {
            WordDefinitionV2::ZhDefinition { content, .. }
            | WordDefinitionV2::ZhSentence { content, .. } => Some(content.text().to_owned()),
            WordDefinitionV2::EnDefinition { .. } | WordDefinitionV2::EnSentence { .. } => None,
        })
        .unwrap_or_default()
}

pub(super) fn reference_issue(
    node_id: Uuid,
    field: &str,
    code: &str,
    message: &str,
) -> DraftValidationIssue {
    DraftValidationIssue {
        step: PersistedWordStep::Meanings,
        node_id,
        field: field.to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        reference_location: None,
        node_location: None,
    }
}
