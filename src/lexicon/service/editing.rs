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
        if affected.is_empty() {
            return Ok(FormsImpactResponseV2 {
                base_revision: current.revision,
                requires_confirmation: false,
                affected,
                confirmation_token: None,
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
        let node_issues = validate_node_identities(entry_id, &proposed, &existing);
        if !node_issues.is_empty() {
            return Err(LexiconServiceError::ValidationFailed(node_issues));
        }

        let affected = forms_impact(&current, &input.content, &meanings);
        if !affected.is_empty() {
            let token = input
                .confirmed_impact_token
                .ok_or_else(|| downstream_required(&affected))?;
            let confirmation = self
                .impacts
                .load(actor_id, token)
                .await
                .map_err(LexiconServiceError::ImpactStore)?
                .ok_or_else(|| downstream_required(&affected))?;
            let expected_hash = sha256_json(&input.content).map_err(serialization_error)?;
            if confirmation.entry_id != entry_id
                || confirmation.base_revision != current.revision
                || confirmation.content_hash != expected_hash
            {
                return Err(downstream_required(&affected));
            }
        }

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
        transaction.commit().await.map_err(database_error)?;
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
        let node_issues = validate_node_identities(entry_id, &proposed, &existing);
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
            | "dialect_variants_invalid"
            | "form_type_invalid"
            | "duplicate_form_type"
            | "base_spelling_mismatch"
            | "spelling_not_trimmed"
            | "spelling_too_long"
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
    let next_nodes = proposed_nodes(next_forms, next_meanings);
    let next_by_id = next_nodes
        .iter()
        .map(|node| (node.id, node))
        .collect::<HashMap<_, _>>();
    let mut affected = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for node in current_nodes {
        if node.node_type != "pos" && node.step != PersistedWordStep::Meanings {
            continue;
        }
        let binding_is_unchanged = next_by_id.get(&node.id).is_some_and(|next| *next == &node);
        if binding_is_unchanged && !changed_pos_ids.contains(&node.id) {
            continue;
        }
        if !seen.insert(node.id) {
            continue;
        }
        affected.push(FormsImpactItemV2 {
            node_id: node.id,
            node_type: node.node_type.to_owned(),
            reason: if node.node_type == "pos" && changed_pos_ids.contains(&node.id) {
                "词性被删除或代码被替换，其下游词义内容将重建".to_owned()
            } else {
                "节点将因词形结构变更从草稿中移除或重新绑定".to_owned()
            },
        });
    }
    affected
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
