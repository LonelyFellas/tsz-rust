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
        let current = LexiconRepository::entry_by_id_for_update(&mut transaction, entry_id)
            .await
            .map_err(repository_error)?
            .ok_or(LexiconServiceError::WordNotFound)?;
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
        if current.archived_at.is_some()
            || current.current_publication_id.is_some()
            || !LexiconRepository::delete_never_published_entry(
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
        let response = self
            .transition_lifecycle(
                actor_id,
                request_id,
                idempotency_key,
                ARCHIVE_SCOPE,
                TargetState::Archived,
                vec![single_target(entry_id, input)],
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
        let response = self
            .transition_lifecycle(
                actor_id,
                request_id,
                idempotency_key,
                RESTORE_SCOPE,
                TargetState::Active,
                vec![single_target(entry_id, input)],
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
        self.transition_lifecycle(
            actor_id,
            request_id,
            idempotency_key,
            ARCHIVE_BATCH_SCOPE,
            TargetState::Archived,
            input.entries,
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
        self.transition_lifecycle(
            actor_id,
            request_id,
            idempotency_key,
            RESTORE_BATCH_SCOPE,
            TargetState::Active,
            input.entries,
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
        let mut words_by_id = HashMap::new();
        let mut affected = 0;
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
            if already_target {
                words_by_id.insert(current.id, current);
                continue;
            }
            ensure_lifecycle_revision(&current, target.base_lifecycle_revision)?;
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
        Ok(response)
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

fn ensure_lifecycle_revision(
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
