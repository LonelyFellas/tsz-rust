use super::*;
use chrono::DateTime;

const ARCHIVE_SCOPE: &str = "lexicon.entry.archive";
const RESTORE_SCOPE: &str = "lexicon.entry.restore";
const ARCHIVE_BATCH_SCOPE: &str = "lexicon.entry.archive_batch";
const RESTORE_BATCH_SCOPE: &str = "lexicon.entry.restore_batch";
const DELETE_BATCH_SCOPE: &str = "lexicon.entry.delete_batch";

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
    /// 生命周期命令只受理 V3 词条；库里出现别的 `content_schema_version` 属于不变量破损。
    pub async fn lifecycle_requires_v3(
        &self,
        entry_ids: &[Uuid],
    ) -> Result<(), LexiconServiceError> {
        let versions = self
            .repository
            .lifecycle_schema_versions(entry_ids)
            .await
            .map_err(repository_error)?;
        if let Some(version) = versions.iter().find(|version| **version != 3) {
            return Err(LexiconServiceError::UnsupportedSchemaVersion(*version));
        }
        Ok(())
    }

    pub async fn delete_draft(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        input: DeleteDraftInput,
        allow_v3: bool,
        is_super_admin: bool,
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
        LexiconRepository::lock_surface_policy_writer(&mut transaction)
            .await
            .map_err(repository_error)?;
        Self::delete_entry_in_transaction(
            &mut transaction,
            actor_id,
            request_id,
            entry_id,
            input.base_revision,
            input.base_lifecycle_revision,
            allow_v3,
            is_super_admin,
            &[],
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }

    /// 批量永久删除（原子）：任意一条不满足条件则整批回滚，语义与
    /// archive-batch / restore-batch 的「要么全成、要么全不动」保持一致。
    /// 永久删除不可逆，部分成功是最坏的失败模式，因此这里不做「跳过失败项」。
    pub async fn delete_draft_batch(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        idempotency_key: Uuid,
        targets: Vec<EntryLifecycleTarget>,
        allow_v3: bool,
        is_super_admin: bool,
    ) -> Result<EntryDeleteBatchResponse, LexiconServiceError> {
        validate_targets(&targets)?;
        let request_hash = sha256_json(&serde_json::json!({
            "operation": "delete_batch",
            "entries": &targets,
        }))
        .map_err(serialization_error)?;
        // 加锁顺序按 id 排序固定下来，避免两个并发批量以相反顺序持锁形成 ABBA。
        let mut ordered = targets.clone();
        ordered.sort_by_key(|target| target.id);
        let entry_ids = ordered.iter().map(|target| target.id).collect::<Vec<_>>();

        let mut transaction = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{DELETE_BATCH_SCOPE}:{actor_id}:{idempotency_key}"))
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if let Some(existing) = LexiconRepository::idempotency(
            &mut transaction,
            DELETE_BATCH_SCOPE,
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

        // 先一次性取全部 surface-context 锁（内部按 BTreeSet 排序），
        // 后续逐条再取同一把锁是 no-op。
        LexiconRepository::lock_surface_contexts(&mut transaction, &entry_ids)
            .await
            .map_err(repository_error)?;
        LexiconRepository::lock_surface_policy_writer(&mut transaction)
            .await
            .map_err(repository_error)?;
        // 批内成员之间的引用会随各自的删除一并消失，提前清掉，
        // 否则先删的那条会撞上后一条的 RESTRICT 外键——结果取决于 id 排序。
        LexiconRepository::clear_intra_batch_references(&mut transaction, &entry_ids)
            .await
            .map_err(repository_error)?;
        for target in &ordered {
            Self::delete_entry_in_transaction(
                &mut transaction,
                actor_id,
                request_id,
                target.id,
                target.base_revision,
                target.base_lifecycle_revision,
                allow_v3,
                is_super_admin,
                &entry_ids,
            )
            .await?;
        }
        let response = EntryDeleteBatchResponse {
            affected: ordered.len(),
        };
        LexiconRepository::insert_idempotent_response(
            &mut transaction,
            DELETE_BATCH_SCOPE,
            actor_id,
            idempotency_key,
            &request_hash,
            (ordered.len() == 1).then_some(ordered[0].id),
            &response,
            200,
        )
        .await
        .map_err(repository_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(response)
    }

    /// 单条永久删除的事务内实现。调用方负责开事务、取 surface-context 锁与提交，
    /// 批量入口据此复用同一套判定，避免两条路径的规则漂移。
    #[allow(clippy::too_many_arguments)]
    async fn delete_entry_in_transaction(
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        base_revision: i64,
        base_lifecycle_revision: i64,
        allow_v3: bool,
        is_super_admin: bool,
        batch_members: &[Uuid],
    ) -> Result<(), LexiconServiceError> {
        LexiconRepository::lock_surface_contexts(transaction, &[entry_id])
            .await
            .map_err(repository_error)?;
        let record = LexiconRepository::entry_by_id_for_update(transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        if record.revision != base_revision {
            return Err(LexiconServiceError::RevisionConflict {
                current_revision: record.revision,
            });
        }
        if record.lifecycle_revision != base_lifecycle_revision {
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
        LexiconRepository::lock_surface_contexts(transaction, &relation_targets)
            .await
            .map_err(repository_error)?;
        let mut surface_keys = crate::lexicon::repository::surface_lock_keys(std::iter::empty());
        surface_keys.extend(
            LexiconRepository::lifecycle_surface_lock_keys(transaction, &[entry_id])
                .await
                .map_err(repository_error)?,
        );
        LexiconRepository::lock_surface_keys(transaction, &surface_keys)
            .await
            .map_err(repository_error)?;
        let record = LexiconRepository::entry_by_id_for_update(transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
        // 普通管理员只能删自己创建的词条；超管不受限。归属先于可删性判定，
        // 避免把「这条能不能删」的信息泄露给无权处置它的管理员。
        if !is_super_admin && record.created_by_admin_id != actor_id {
            return Err(LexiconServiceError::EntryDeleteForbidden);
        }
        // 归档态可删：垃圾桶是软删除的中间站，不是终点。真正不可删的是「发布过」——
        // publication 历史必须保留（delete_never_published_entry 里还有一层 NOT EXISTS 兜底）。
        if record.current_publication_id.is_some() {
            return Err(LexiconServiceError::EntryNotDeletable);
        }
        if LexiconRepository::has_inbound_prebound_relations(transaction, entry_id)
            .await
            .map_err(repository_error)?
        {
            return Err(LexiconServiceError::EntryHasInboundPreboundRelations);
        }
        LexiconRepository::retire_v3_draft_surface_projection(
            transaction,
            entry_id,
            record.revision + 1,
        )
        .await
        .map_err(repository_error)?;
        if !LexiconRepository::delete_never_published_entry(
            transaction,
            actor_id,
            request_id,
            entry_id,
            record.revision,
            batch_members,
        )
        .await
        .map_err(repository_error)?
        {
            return Err(LexiconServiceError::EntryNotDeletable);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn archive(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        idempotency_key: Uuid,
        input: EntryLifecycleInput,
        allow_v3: bool,
        is_super_admin: bool,
    ) -> Result<AdminWordV3Envelope, LexiconServiceError> {
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
                is_super_admin,
            )
            .await?;
        one_word(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn restore(
        &self,
        actor_id: Uuid,
        request_id: Uuid,
        entry_id: Uuid,
        idempotency_key: Uuid,
        input: EntryLifecycleInput,
        allow_v3: bool,
        is_super_admin: bool,
    ) -> Result<AdminWordV3Envelope, LexiconServiceError> {
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
                is_super_admin,
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
        is_super_admin: bool,
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
            allow_v3,
            is_super_admin,
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
        is_super_admin: bool,
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
            allow_v3,
            is_super_admin,
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
        is_super_admin: bool,
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
            // 整批原子：一条越权就拒掉整批，与 delete_draft_batch 同口径。
            ensure_draft_writable(&record, actor_id, is_super_admin)?;
            ensure_lifecycle_schema_capability(record.content_schema_version, allow_v3)?;
            let record_revision = record.revision;
            let record_lifecycle_revision = record.lifecycle_revision;
            let current = self.get_v3(record.id).await?;
            if current.revision != record_revision
                || current.lifecycle_revision != record_lifecycle_revision
            {
                return Err(invariant_record());
            }
            ensure_word_revision(&current, target.base_revision)?;
            let already_target = match target_state {
                TargetState::Archived => current.archived_at.is_some(),
                TargetState::Active => current.archived_at.is_none(),
            };
            if !already_target {
                ensure_word_lifecycle_revision(&current, target.base_lifecycle_revision)?;
            }
            pending.push((target, current, already_target));
        }

        let pending_entry_ids = pending
            .iter()
            .map(|(_, word, _)| word.id)
            .collect::<Vec<_>>();
        let mut affected_contexts = pending
            .iter()
            .flat_map(|(_, word, _)| word_relation_target_entry_ids(word))
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

        let v3_entry_ids = pending
            .iter()
            .map(|(_, word, _)| word.id)
            .collect::<Vec<_>>();
        let _entry_ids = pending
            .iter()
            .filter(|(_, _, already_target)| {
                target_state == TargetState::Archived || !*already_target
            })
            .map(|(_, word, _)| word.id)
            .collect::<Vec<_>>();
        let mut surface_keys = crate::lexicon::repository::surface_lock_keys(std::iter::empty());
        for (_, word, _) in &pending {
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
        for entry_id in &v3_entry_ids {
            surface_keys.extend(
                self.v3_initial_headword_surface_lock_keys(&mut transaction, *entry_id)
                    .await?,
            );
        }
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
                LexiconRepository::lock_current_outbound_sense_ref_targets_for_entry(
                    &mut transaction,
                    current.id,
                )
                .await
                .map_err(repository_error)?;
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
            if target_state == TargetState::Active {
                restored_audit_targets.push((next.id, next.revision));
            }
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
        pending: &[(EntryLifecycleTarget, AdminWordV3, bool)],
        all_targets: &[EntryLifecycleTarget],
        token: Option<&str>,
    ) -> Result<Option<VerifiedSurfaceConfirmation>, LexiconServiceError> {
        let additions = crate::lexicon::visibility::headword_memberships(&[]);
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
        let _active_ids = transitions
            .iter()
            .flat_map(|item| item.after_active_ids.iter().copied())
            .collect::<std::collections::HashSet<_>>();
        let items = std::collections::BTreeMap::new();
        let contexts: std::collections::BTreeMap<Uuid, MatchedEntryContextV2> =
            std::collections::BTreeMap::new();
        let v3_contribution = self
            .v3_restore_surface_contribution(transaction, pending)
            .await?;
        let mut items = items;
        for item in &v3_contribution.items {
            if items.insert(item.match_id.clone(), item.clone()).is_some() {
                return Err(invariant_record());
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
            if let Some(token) = token {
                self.verify_v3_surface_owner(token, actor_id, command, owner_context)
                    .await?;
                return Err(LexiconServiceError::SurfaceMatchesChangedWithoutSnapshot);
            }
            return Ok(None);
        }
        let _ = contexts;
        let v3_page_data = self
            .v3_restore_page_data(transaction, &items, &v3_contribution.page_items)
            .await?;
        let contexts = super::v3_surface::v3_restore_synthetic_contexts(&v3_page_data);
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
        let entry_state_evidence = pending
            .iter()
            .map(|(_, word, already_target)| {
                serde_json::json!({
                    "schema_version": 3,
                    "entry_id": word.id,
                    "current_revision": word.revision,
                    "current_lifecycle_revision": word.lifecycle_revision,
                    "published_revision": word.published_revision,
                    "already_active": already_target,
                })
            })
            .collect::<Vec<_>>();
        let mut owner_bundle = serde_json::json!({
            "command": if scope == RESTORE_SCOPE { "restore_entry" } else { "restore_entries_batch" },
            "selection": selection,
            "entry_state_evidence": entry_state_evidence,
            "transitions": transitions,
            "match_ids": items.iter().map(|item| &item.match_id).collect::<Vec<_>>(),
            "confirmation_reasons": confirmation_reasons,
        });
        owner_bundle[crate::lexicon::surface_snapshot::V3_SURFACE_PAGE_DATA_KEY] =
            serde_json::to_value(&v3_page_data).map_err(serialization_error)?;
        owner_bundle["v3_candidate_evidence"] =
            serde_json::to_value(&v3_contribution.candidate_evidence)
                .map_err(serialization_error)?;
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
                let page =
                    crate::lexicon::surface_snapshot::surface_page_v3(snapshot.page, &owner_bundle)
                        .map_err(LexiconServiceError::SurfaceSnapshot)?;
                return Err(LexiconServiceError::SurfaceMatchesChangedV3(Box::new(page)));
            }
            Err(error) => return Err(LexiconServiceError::SurfaceSnapshot(error)),
        };
        if !policy.enabled && visibility_required {
            let snapshot = self
                .surface_snapshots
                .create(create_snapshot())
                .await
                .map_err(LexiconServiceError::SurfaceSnapshot)?;
            let page =
                crate::lexicon::surface_snapshot::surface_page_v3(snapshot.page, &owner_bundle)
                    .map_err(LexiconServiceError::SurfaceSnapshot)?;
            return Err(
                LexiconServiceError::MultipleActiveExactHeadwordPublicationsNotEnabledV3(Box::new(
                    page,
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
            let page =
                crate::lexicon::surface_snapshot::surface_page_v3(snapshot.page, &owner_bundle)
                    .map_err(LexiconServiceError::SurfaceSnapshot)?;
            return Err(LexiconServiceError::SurfaceMatchesChangedV3(Box::new(page)));
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
) -> Result<AdminWordV3Envelope, LexiconServiceError> {
    Ok(AdminWordV3Envelope {
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

fn lifecycle_word(mut word: AdminWordV3, target_state: TargetState, actor_id: Uuid) -> AdminWordV3 {
    let now = Utc::now();
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

fn ensure_word_revision(word: &AdminWordV3, base_revision: i64) -> Result<(), LexiconServiceError> {
    if word.revision != base_revision {
        return Err(LexiconServiceError::RevisionConflict {
            current_revision: word.revision,
        });
    }
    Ok(())
}

fn ensure_word_lifecycle_revision(
    word: &AdminWordV3,
    base_lifecycle_revision: i64,
) -> Result<(), LexiconServiceError> {
    if word.lifecycle_revision != base_lifecycle_revision {
        return Err(LexiconServiceError::LifecycleRevisionConflict {
            current_lifecycle_revision: word.lifecycle_revision,
        });
    }
    Ok(())
}

/// 生命周期命令要锁住被本词条引用的上下文；待物化的关联词没有目标词条，自然也不锁。
fn word_relation_target_entry_ids(word: &AdminWordV3) -> Vec<Uuid> {
    let mut entry_ids = word
        .meanings
        .pos
        .iter()
        .flat_map(|pos| pos.senses.iter())
        .flat_map(|sense| sense.relations.iter())
        .filter_map(|relation| relation.target_word_id)
        .collect::<Vec<_>>();
    entry_ids.sort_unstable();
    entry_ids.dedup();
    entry_ids
}

fn semantic(field: &'static str, message: &'static str) -> LexiconServiceError {
    LexiconServiceError::UnprocessableField { field, message }
}
