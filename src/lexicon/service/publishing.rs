use super::*;
use crate::lexicon::dto::RichTextV1;

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
            // 不物化的调用点：没有本次刚绑定的关联词。
            &std::collections::HashSet::new(),
        )
        .await?;
        issues.extend(reference_resolution.issues);
        transaction.commit().await.map_err(database_error)?;
        Ok(DraftValidationResponse {
            schema_version: 2,
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
        allow_v3_targets: bool,
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

        LexiconRepository::lock_surface_contexts(&mut transaction, &[entry_id])
            .await
            .map_err(repository_error)?;
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
        let mut affected_contexts = relation_target_entry_ids(&word.meanings);
        if let Some(publication_id) = current_publication_id {
            affected_contexts.extend(
                LexiconRepository::publication_relation_target_entry_ids(
                    &mut transaction,
                    &[publication_id],
                )
                .await
                .map_err(repository_error)?,
            );
        }
        LexiconRepository::lock_surface_contexts(&mut transaction, &affected_contexts)
            .await
            .map_err(repository_error)?;
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
            Self::hydrate_sentence_associations_in(&mut transaction, &mut word).await?;
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

        // 待物化的关联词必须先长成真实词条，随后的 Verify 复核才有目标可解析。
        // 放在同一个事务里：发布失败就一起回滚，不会留下没人引用的占位。
        let (materialization_issues, newly_bound_relations) = self
            .resolve_pending_relation_targets(
                &mut transaction,
                actor_id,
                request_id,
                entry_id,
                &mut word.meanings,
                PendingRelationResolution::Materialize,
            )
            .await?;
        if !materialization_issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(
                materialization_issues,
            ));
        }
        let reference_resolution = resolve_meaning_references(
            &mut transaction,
            entry_id,
            &mut word.meanings,
            ReferenceResolutionMode::Verify,
            true,
            &newly_bound_relations,
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
            Self::hydrate_sentence_associations_in(&mut transaction, &mut word).await?;
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
        // 例句自动关联：解析不出目标、有歧义、词面不合法都只跳过那个词，发布照常成功。
        // 与上面的关联词物化相反——那边是管理员显式录入的意图，缺目标必须拦下。
        // 挂在这里而不是两条早返回路径上：那两条不产出新 publication，正文与上次发布
        // 逐字相同，解析结果不会变。
        Self::refresh_sentence_associations(
            &mut transaction,
            entry_id,
            &word.meanings,
            allow_v3_targets,
        )
        .await?;
        Self::hydrate_sentence_associations_in(&mut transaction, &mut word).await?;
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

        LexiconRepository::lock_surface_contexts(&mut transaction, &[entry_id])
            .await
            .map_err(repository_error)?;
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
            Self::hydrate_sentence_associations_in(&mut transaction, &mut word).await?;
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

        let mut publication_word = v2_publication_snapshot(publication.snapshot.clone())?;
        if publication.entry_id != entry_id || publication_word.id != entry_id {
            return Err(invariant_record());
        }
        publication_word.status = AdminWordStatus::Published;
        publication_word.archived_at = None;
        publication_word.archived_by = None;
        publication_word.published_revision = Some(publication.source_revision);
        publication_word.published_at = Some(publication.published_at);
        let mut affected_contexts = relation_target_entry_ids(&word.meanings);
        let publication_ids = previous_publication_id
            .into_iter()
            .chain(std::iter::once(publication_id))
            .collect::<Vec<_>>();
        affected_contexts.extend(
            LexiconRepository::publication_relation_target_entry_ids(
                &mut transaction,
                &publication_ids,
            )
            .await
            .map_err(repository_error)?,
        );
        LexiconRepository::lock_surface_contexts(&mut transaction, &affected_contexts)
            .await
            .map_err(repository_error)?;
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
        Self::hydrate_sentence_associations_in(&mut transaction, &mut word).await?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    target_publication_id: Option<Uuid>,
    target_content_scope: PublicationTargetContentScope,
    target_revision: i64,
    headword: String,
    gloss: String,
    available: bool,
}

#[derive(Debug, Default)]
pub(super) struct MeaningReferenceResolution {
    pub(super) issues: Vec<DraftValidationIssue>,
    pub(super) publication_references: Vec<NewPublicationSenseReference>,
}

/// 待物化关联词的处理强度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingRelationResolution {
    /// 草稿保存：同名词条已经存在就顺带绑定，不建条、也不报错。
    ///
    /// 草稿不该因为目标还没建、或者别人的草稿没写完而存不下来；绑不上的就继续留在
    /// 待物化形态，等发布时物化。这条路让「发布建出占位之后草稿还显示待建」自然收敛。
    BindExisting,
    /// 发布：目标必须落定——缺就建，建不出来（目标词条没有词义可指）就报错。
    Materialize,
}

/// 一个待建的关联词目标，连同引用了它的关联词节点。
///
/// 节点 id 必须带着走：出错时要把 issue 锚回具体那一条关联词，否则前端只能指向词条
/// 本身，管理员不知道该改哪一行；多个目标同时出错还会产出无法区分的重复 issue。
struct PendingRelationTarget {
    display: String,
    kind: EntryKind,
    gloss: Option<String>,
    relation_ids: Vec<Uuid>,
}

impl LexiconService {
    /// 处理草稿里待物化的关联词：能绑的绑上，`Materialize` 模式下缺的还会建出来。
    ///
    /// **只有 `Materialize` 会建条**，而它只从发布链路调用。草稿保存走
    /// `BindExisting`，一个词条都不会造出来——错字和被放弃的草稿因此永远不会落成词条，
    /// 走到发布那一步的每个占位背后都是一次经过校验并由管理员确认的编辑意图。
    ///
    /// 同名词条已存在时绑过去而不是再建一条：两个同名词条会撞
    /// `lexicon_entry_headword_keys_unique_idx`，语义上本来也该是同一个词。
    pub(super) async fn resolve_pending_relation_targets(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        meanings: &mut DraftMeaningsStepContent,
        resolution: PendingRelationResolution,
    ) -> Result<(Vec<DraftValidationIssue>, std::collections::HashSet<Uuid>), LexiconServiceError>
    {
        let mut pending = BTreeMap::<String, PendingRelationTarget>::new();
        let mut issues = Vec::new();
        for pos in &mut meanings.pos {
            for sense in &mut pos.senses {
                for relation in &mut sense.relations {
                    if relation.bound_target().is_some() {
                        continue;
                    }
                    // 词面在草稿保存时已经校验过；这里再拒一次不合法的，交给随后的
                    // Verify 复核统一报错，不在物化里制造第二套错误码。
                    let Some(raw) = relation.pending_target_headword.as_deref() else {
                        continue;
                    };
                    let Ok(normalized) =
                        crate::lexicon::normalization::NormalizedHeadword::parse(raw)
                    else {
                        continue;
                    };
                    let kind = if normalized.display.contains(' ') {
                        EntryKind::Phrase
                    } else {
                        EntryKind::Word
                    };
                    let gloss = relation
                        .pending_target_gloss
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned);
                    relation.pending_target_gloss = gloss.clone();
                    if gloss.as_deref().is_some_and(|value| {
                        value.contains('\0')
                            || value.chars().count()
                                > crate::lexicon::rich_text::MAX_RICH_TEXT_CODEPOINTS
                    }) {
                        issues.push(reference_issue(
                            relation.id,
                            "pending_target_gloss",
                            "relation_pending_gloss_invalid",
                            "预定义词义不能超过 5000 个字符",
                        ));
                        continue;
                    }
                    let target =
                        pending
                            .entry(normalized.key)
                            .or_insert_with(|| PendingRelationTarget {
                                display: normalized.display,
                                kind,
                                gloss: gloss.clone(),
                                relation_ids: Vec::new(),
                            });
                    if target.gloss.is_none() {
                        target.gloss = gloss.clone();
                    } else if gloss.is_some() && gloss != target.gloss {
                        issues.push(reference_issue(
                            relation.id,
                            "pending_target_gloss",
                            "relation_pending_gloss_conflict",
                            "同一待建关联词不能填写不同的预定义词义",
                        ));
                    }
                    target.relation_ids.push(relation.id);
                }
            }
        }
        if !issues.is_empty() {
            return Ok((issues, std::collections::HashSet::new()));
        }
        if pending.is_empty() {
            return Ok((Vec::new(), std::collections::HashSet::new()));
        }

        let materializing = resolution == PendingRelationResolution::Materialize;
        // 只有真要建条时才解析词性目录——绑定模式一个词条都不会造出来。
        let stub_part = if materializing {
            let parts = LexiconRepository::catalog_parts_for_reference(
                tx,
                std::slice::from_ref(&STUB_PART_OF_SPEECH.to_owned()),
            )
            .await
            .map_err(repository_error)?;
            Some(parts.into_iter().next().ok_or_else(invariant_record)?)
        } else {
            None
        };

        let mut bindings = HashMap::<String, (Uuid, Uuid)>::new();
        for (normalized_key, target) in pending {
            let existing = if materializing {
                {
                    // 先锁词面再查同名：否则两个并发发布会各自查到「不存在」，各建一条
                    // 同名占位，第二条撞唯一索引变成内部错误。锁键只由词面决定，
                    // 不需要为此造出一整个占位聚合。
                    let lock_keys = crate::lexicon::surface::normalize_surface_scopes(
                        &target.display,
                        Dialect::Common,
                    )
                    .map_err(surface_projection_error)?
                    .into_iter()
                    .map(|scope| crate::lexicon::repository::SurfaceLockKey {
                        language: "en".to_owned(),
                        dialect_scope: scope.dialect_scope.to_owned(),
                        normalized_surface: scope.normalized_surface,
                    })
                    .collect::<Vec<_>>();
                    LexiconRepository::lock_surface_keys(tx, &lock_keys)
                        .await
                        .map_err(repository_error)?;
                    LexiconRepository::find_entry_by_headword_key_for_update(
                        tx,
                        target.kind,
                        &normalized_key,
                    )
                    .await
                    .map_err(repository_error)?
                }
            } else {
                // 绑定模式不建条，也就不需要防并发建重；加锁反而会让保存 A 的事务
                // 占着 B 的行到提交，把 B 自己的保存挡在外面。
                LexiconRepository::find_entry_by_headword_key(tx, target.kind, &normalized_key)
                    .await
                    .map_err(repository_error)?
            };

            let bound = match existing {
                // 关联词写成了本词条自己的主词。绑上去只会在随后的复核里以
                // relation_self_target 报错，而那条错误指向管理员根本没填过的
                // target_word_id；直接在这里按他实际写的字段报出来。
                Some(found) if found.entry_id == entry_id => {
                    issues.extend(target.relation_ids.iter().map(|relation_id| {
                        reference_issue(
                            *relation_id,
                            "pending_target_headword",
                            "relation_self_target",
                            "关联词不能指向当前词条自身",
                        )
                    }));
                    continue;
                }
                // 同名词条已归档。这里必须报错，且草稿保存就报——别的绑不上的情形
                // （目标还没建、目标还没写词义）都会自愈，留着待物化等下一次即可，
                // 归档不会：关联词搜索只搜已发布未归档词条，管理员根本看不见它，
                // 绑上去就是「草稿存得下、发布必被拒」，而那时待建词面已被清空，
                // 他重填同一个词还会再绑上来。归档词条又占着词头唯一键，绕过它
                // 另建同名新条会撞 lexicon_entry_headword_keys_unique_idx。
                // 趁词面还在手上报出来，管理员才能改词面、或先去恢复那条词条。
                Some(found) if found.is_archived => {
                    issues.extend(target.relation_ids.iter().map(|relation_id| {
                        reference_issue(
                            *relation_id,
                            "pending_target_headword",
                            "relation_target_archived",
                            "同名词条已归档，请先恢复它再关联，或改指向其他词",
                        )
                    }));
                    continue;
                }
                Some(_) if target.gloss.is_some() => {
                    issues.extend(target.relation_ids.iter().map(|relation_id| {
                        reference_issue(
                            *relation_id,
                            "pending_target_gloss",
                            "relation_pending_gloss_target_exists",
                            "同名词条已存在，请选择它的具体词义，预定义词义不会覆盖已有内容",
                        )
                    }));
                    continue;
                }
                Some(found) => {
                    let sense = LexiconRepository::first_draft_sense(tx, found.entry_id)
                        .await
                        .map_err(repository_error)?;
                    match sense {
                        Some(sense_id) => (found.entry_id, sense_id),
                        // 目标词条已存在但还停在 forms 步骤，没有义项可指。草稿保存不该
                        // 因此失败，留着待物化即可；发布必须落定，就报出可操作的错误，
                        // 让管理员先去把那边的词义补上，而不是替他改别人的草稿。
                        None if !materializing => continue,
                        None => {
                            issues.extend(target.relation_ids.iter().map(|relation_id| {
                                reference_issue(
                                    *relation_id,
                                    "pending_target_headword",
                                    "relation_target_has_no_sense",
                                    "关联词目标词条还没有词义，请先补全后再发布",
                                )
                            }));
                            continue;
                        }
                    }
                }
                // 绑定模式下目标还不存在：继续留在待物化形态，等发布时物化。
                None if !materializing => continue,
                None => {
                    let part = stub_part.as_ref().ok_or_else(invariant_record)?;
                    let stub = build_relation_target_stub(&target, &normalized_key, actor_id, part);
                    let sources = crate::lexicon::repository::surface_projection_sources(&stub)
                        .map_err(surface_projection_error)?;
                    let sense_id = stub
                        .meanings
                        .pos
                        .first()
                        .and_then(|pos| pos.senses.first())
                        .map(|sense| sense.id)
                        .ok_or_else(invariant_record)?;
                    let stub_id = stub.id;
                    let part_map = HashMap::from([(part.code.clone(), part.id)]);
                    LexiconRepository::insert_relation_target_entry(
                        tx, &stub, actor_id, request_id, &part_map, entry_id,
                    )
                    .await
                    .map_err(repository_error)?;
                    LexiconRepository::replace_surface_projection(
                        tx,
                        stub_id,
                        stub.revision,
                        crate::lexicon::repository::SurfaceContentScope::Draft,
                        None,
                        &[],
                        &sources,
                    )
                    .await
                    .map_err(repository_error)?;
                    (stub_id, sense_id)
                }
            };
            bindings.insert(normalized_key, bound);
        }

        let mut newly_bound = std::collections::HashSet::new();
        for pos in &mut meanings.pos {
            for sense in &mut pos.senses {
                for relation in &mut sense.relations {
                    if relation.bound_target().is_some() {
                        continue;
                    }
                    let Some(raw) = relation.pending_target_headword.as_deref() else {
                        continue;
                    };
                    let Ok(normalized) =
                        crate::lexicon::normalization::NormalizedHeadword::parse(raw)
                    else {
                        continue;
                    };
                    let Some((target_word_id, target_sense_id)) = bindings.get(&normalized.key)
                    else {
                        continue;
                    };
                    relation.target_word_id = Some(*target_word_id);
                    relation.target_sense_id = Some(*target_sense_id);
                    relation.pending_target_headword = None;
                    relation.pending_target_gloss = None;
                    newly_bound.insert(relation.id);
                }
            }
        }
        Ok((issues, newly_bound))
    }
}

/// 占位词条的默认词性。关联词的外键钉在**义项**节点上，所以占位不能是空壳——
/// 至少要有一个词性和一个义项供指向，词性只能给个默认值。
const STUB_PART_OF_SPEECH: &str = "noun";

/// 按最省的形状造一个占位词条：unified 主词、一个词性、一个空义项。
///
/// `detection_snapshot` 是库层 NOT NULL 的，而占位并没有经过检测，所以合成一份
/// `clear` 的快照如实记录「没有检测过」，而不是伪造一次命中。
fn build_relation_target_stub(
    target: &PendingRelationTarget,
    normalized_key: &str,
    actor_id: Uuid,
    part: &CatalogPartRecord,
) -> AdminWordV2 {
    let word_id = Uuid::now_v7();
    let now = Utc::now();
    let headwords = WordHeadwordsV2::Unified {
        common: target.display.clone(),
    };
    let forms = crate::lexicon::service::entry::build_suggested_forms(
        &headwords,
        std::slice::from_ref(part),
    );
    let sense_group_id = Uuid::now_v7();
    let mut meanings = DraftMeaningsStepContent {
        sense_groups: vec![SenseGroupV2 {
            id: sense_group_id,
            name_zh: String::new(),
            name_en: String::new(),
        }],
        pos: forms
            .pos
            .iter()
            .map(|forms_pos| {
                crate::lexicon::service::entry::build_initial_pos_meanings(
                    word_id,
                    &headwords,
                    forms_pos,
                    sense_group_id,
                )
            })
            .collect(),
    };
    if let Some(gloss) = target.gloss.as_ref()
        && let Some(WordDefinitionV2::ZhDefinition { content, .. }) = meanings
            .pos
            .first_mut()
            .and_then(|pos| pos.senses.first_mut())
            .and_then(|sense| sense.definitions.first_mut())
    {
        *content = RichText::V1(RichTextV1 {
            version: 1,
            text: gloss.clone(),
            spans: Vec::new(),
            liaisons: Vec::new(),
        });
    }
    AdminWordV2 {
        schema_version: 2,
        id: word_id,
        language: "en".to_owned(),
        kind: target.kind,
        status: AdminWordStatus::Draft,
        revision: 1,
        lifecycle_revision: 1,
        published_revision: None,
        has_unpublished_changes: false,
        headwords: headwords.clone(),
        frequency: None,
        detection_snapshot: WordDetectionSnapshotV2 {
            detection_id: Uuid::now_v7(),
            request: DetectionRequestEcho {
                language: "en".to_owned(),
                headword: target.display.clone(),
            },
            normalized_headword: normalized_key.to_owned(),
            entry_kind: target.kind,
            matched_dialect: Dialect::Common,
            builtin_dictionary_status: "not_found".to_owned(),
            smart_dictionary: WordDetectionSnapshotSmartDictionaryV2::Clear {
                surface_warning: None,
            },
            headwords,
            suggested_pos: vec![part.code.clone()],
            dictionary_provider: None,
            dictionary_coverage: None,
            dictionary_provenance: None,
            detected_at: now,
        },
        forms,
        meanings,
        completed_steps: vec![PersistedWordStep::Basics],
        max_reachable_step: WordCreationStep::Forms,
        created_by: actor_id,
        created_at: now,
        updated_at: now,
        archived_at: None,
        archived_by: None,
        published_at: None,
    }
}

pub(super) async fn resolve_meaning_references(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    entry_id: Uuid,
    meanings: &mut DraftMeaningsStepContent,
    mode: ReferenceResolutionMode,
    lock_for_publish: bool,
    newly_bound: &std::collections::HashSet<Uuid>,
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
                let Some((target_entry_id, target_sense_id)) = relation.bound_target() else {
                    // 待物化：目标词条还不存在，这里只校验词面本身立不立得住。
                    // 真正建条发生在发布事务里（materialize_pending_relation_targets），
                    // 所以 Verify 模式下不该再见到这种形态。
                    if let Some(issue) = pending_relation_issue(relation, lock_for_publish) {
                        issues.push(issue);
                    }
                    continue;
                };
                if target_entry_id == entry_id {
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
                        target_entry_id,
                        target_sense_id,
                    },
                    kind: ReferenceUseKind::Relation,
                    external: true,
                });
            }
        }
    }

    let relation_requested = uses
        .iter()
        .filter(|usage| usage.kind == ReferenceUseKind::Relation)
        .map(|usage| usage.target)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let context_requested = uses
        .iter()
        .filter(|usage| usage.kind == ReferenceUseKind::SentenceContext)
        .map(|usage| usage.target)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let relation_records = if lock_for_publish {
        LexiconRepository::resolve_relation_targets_for_publish(tx, &relation_requested).await
    } else {
        LexiconRepository::resolve_relation_targets(tx, &relation_requested).await
    }
    .map_err(repository_error)?;
    let context_records = if lock_for_publish {
        LexiconRepository::resolve_current_published_senses_for_publish(tx, &context_requested)
            .await
    } else {
        LexiconRepository::resolve_current_published_senses(tx, &context_requested).await
    }
    .map_err(repository_error)?;
    let mut resolved = HashMap::new();
    for record in relation_records {
        let key = SenseTargetKey {
            target_entry_id: record.target_entry_id,
            target_sense_id: record.target_sense_id,
        };
        resolved.insert(
            (ReferenceUseKind::Relation, key),
            relation_target_snapshot(&record)?,
        );
    }
    for record in context_records {
        let key = SenseTargetKey {
            target_entry_id: record.target_entry_id,
            target_sense_id: record.target_sense_id,
        };
        let (headword, gloss) = published_sense_snapshot(&record)?;
        resolved.insert(
            (ReferenceUseKind::SentenceContext, key),
            ResolvedReferenceSnapshot {
                target_publication_id: Some(record.target_publication_id),
                target_content_scope: PublicationTargetContentScope::Publication,
                target_revision: record.target_revision,
                headword,
                gloss,
                available: true,
            },
        );
    }

    for usage in &uses {
        let snapshot = resolved.get(&(usage.kind, usage.target));
        let accepted = snapshot.is_some_and(|snapshot| snapshot.available);
        if accepted {
            continue;
        }
        let (field, code, message) = match usage.kind {
            ReferenceUseKind::Relation => (
                "target_sense_id",
                "relation_target_unavailable",
                "关联词目标必须是未归档词条当前草稿或当前发布中的有效词义",
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
                let Some((target_entry_id, target_sense_id)) = relation.bound_target() else {
                    continue;
                };
                let key = SenseTargetKey {
                    target_entry_id,
                    target_sense_id,
                };
                let Some(snapshot) = resolved.get(&(ReferenceUseKind::Relation, key)) else {
                    continue;
                };
                match mode {
                    ReferenceResolutionMode::Canonicalize => {
                        relation.target_headword = Some(snapshot.headword.clone());
                        relation.target_gloss = Some(snapshot.gloss.clone());
                    }
                    // 本次事务刚物化并绑定的关联词没有可比的旧快照——它就是权威版本，
                    // 直接落快照。过期检查针对的是「目标内容在你保存之后变了」，
                    // 对刚长出来的目标不成立。
                    ReferenceResolutionMode::Verify if newly_bound.contains(&relation.id) => {
                        relation.target_headword = Some(snapshot.headword.clone());
                        relation.target_gloss = Some(snapshot.gloss.clone());
                    }
                    ReferenceResolutionMode::Verify => {
                        if snapshot.available
                            && (relation.target_headword.as_deref()
                                != Some(snapshot.headword.as_str())
                                || relation.target_gloss.as_deref()
                                    != Some(snapshot.gloss.as_str()))
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
        let Some(snapshot) = resolved.get(&(usage.kind, usage.target)) else {
            continue;
        };
        if !snapshot.available {
            continue;
        }
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
                target_content_scope: snapshot.target_content_scope,
                target_revision: snapshot.target_revision,
            });
        }
    }

    Ok(MeaningReferenceResolution {
        issues,
        publication_references,
    })
}

fn relation_target_snapshot(
    record: &ResolvedRelationTargetRecord,
) -> Result<ResolvedReferenceSnapshot, LexiconServiceError> {
    if let (Some(target_publication_id), Some(snapshot), Some(target_revision)) = (
        record.target_publication_id,
        record.published_snapshot.as_ref(),
        record.published_revision,
    ) {
        let published = ResolvedSenseTargetRecord {
            target_entry_id: record.target_entry_id,
            target_sense_id: record.target_sense_id,
            target_publication_id,
            target_revision,
            snapshot: snapshot.clone(),
        };
        let (headword, gloss) = published_sense_snapshot(&published)?;
        return Ok(ResolvedReferenceSnapshot {
            target_publication_id: Some(target_publication_id),
            target_content_scope: PublicationTargetContentScope::Publication,
            target_revision,
            headword,
            gloss,
            available: !record.target_archived,
        });
    }

    let headword = draft_target_headword(record)?;
    let meanings: DraftMeaningsStepContent =
        serde_json::from_value(record.draft_meanings.clone()).map_err(serialization_error)?;
    let sense = meanings
        .pos
        .iter()
        .flat_map(|pos| &pos.senses)
        .find(|sense| sense.id == record.target_sense_id);
    Ok(ResolvedReferenceSnapshot {
        target_publication_id: None,
        target_content_scope: PublicationTargetContentScope::Draft,
        target_revision: record.target_revision,
        headword,
        gloss: sense.map(published_sense_gloss).unwrap_or_default(),
        available: !record.target_archived && !record.target_removed && sense.is_some(),
    })
}

fn draft_target_headword(
    record: &ResolvedRelationTargetRecord,
) -> Result<String, LexiconServiceError> {
    match record.content_schema_version {
        2 => match record.headword_mode.as_deref() {
            Some("unified") => record.common_headword.clone().ok_or_else(invariant_record),
            Some("distinguish") => match record.source_dialect.as_deref() {
                Some("uk") => record.uk_headword.as_ref().zip(record.us_headword.as_ref()),
                Some("us") => record.us_headword.as_ref().zip(record.uk_headword.as_ref()),
                _ => None,
            }
            .map(|(first, second)| format!("{first} / {second}"))
            .ok_or_else(invariant_record),
            _ => Err(invariant_record()),
        },
        3 => record
            .presentation_label
            .clone()
            .ok_or_else(invariant_record),
        version => Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
    }
}

pub(super) fn published_sense_snapshot(
    record: &ResolvedSenseTargetRecord,
) -> Result<(String, String), LexiconServiceError> {
    let version = record
        .snapshot
        .get("schema_version")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i16::try_from(value).ok())
        .unwrap_or(-1);
    let (headword, meanings) = match version {
        2 => {
            let word = v2_publication_snapshot(record.snapshot.clone())?;
            (published_word_headword(&word), word.meanings)
        }
        3 => {
            let word: AdminWordV3 =
                serde_json::from_value(record.snapshot.clone()).map_err(serialization_error)?;
            let meanings: DraftMeaningsStepContent = serde_json::from_value(
                serde_json::to_value(word.meanings).map_err(serialization_error)?,
            )
            .map_err(serialization_error)?;
            (word.presentation.label, meanings)
        }
        version => return Err(LexiconServiceError::UnsupportedSchemaVersion(version)),
    };
    let sense = meanings
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

/// 校验一条待物化关联词。
///
/// 词面本身永远要校验——归一化 + 字符集，与主词录入同一把尺子，否则「中文近义词」
/// 会绕过 headword 校验从关联词这条路溜进词库。
///
/// `materialized_expected` 只有真正的发布链路会传 true：它在复核之前已经物化过一遍，
/// 走到这里还剩待物化形态就说明物化漏了，按引用不可用拦下而不是放行。草稿校验接口
/// （同样是 Verify 模式）不物化，待物化在那里是合法的草稿状态。
fn pending_relation_issue(
    relation: &WordRelationV2,
    materialized_expected: bool,
) -> Option<DraftValidationIssue> {
    if materialized_expected {
        return Some(reference_issue(
            relation.id,
            "target_sense_id",
            "relation_target_unavailable",
            "关联词目标必须是未归档词条当前草稿或当前发布中的有效词义",
        ));
    }
    if relation.pending_target_headword.is_none() && relation.pending_target_gloss.is_some() {
        return Some(reference_issue(
            relation.id,
            "pending_target_gloss",
            "relation_pending_gloss_without_headword",
            "预定义词义只能用于待建关联词",
        ));
    }
    if relation
        .pending_target_gloss
        .as_deref()
        .is_some_and(|value| {
            value.contains('\0')
                || value.chars().count() > crate::lexicon::rich_text::MAX_RICH_TEXT_CODEPOINTS
        })
    {
        return Some(reference_issue(
            relation.id,
            "pending_target_gloss",
            "relation_pending_gloss_invalid",
            "预定义词义不能超过 5000 个字符",
        ));
    }
    let headword = relation.pending_target_headword.as_deref().unwrap_or("");
    crate::lexicon::normalization::NormalizedHeadword::parse(headword)
        .err()
        .map(|_| {
            reference_issue(
                relation.id,
                "pending_target_headword",
                "relation_pending_headword_invalid",
                "待建关联词的词面必须是合法英文词条名",
            )
        })
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
