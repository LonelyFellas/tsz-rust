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
                })
                .collect();
            return Err(LexiconServiceError::ValidationFailed(issues));
        }

        word.status = AdminWordStatus::Published;
        word.published_revision = Some(word.revision);
        word.has_unpublished_changes = false;
        word.published_at = Some(Utc::now());
        LexiconRepository::insert_publication(
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
        transaction.commit().await.map_err(database_error)?;
        Ok(AdminWordV2Envelope { word })
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
    match &word.headwords {
        WordHeadwordsV2::Unified { common } => common.clone(),
        WordHeadwordsV2::Distinguish { uk, us, .. } => format!("{uk} / {us}"),
    }
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
    }
}
