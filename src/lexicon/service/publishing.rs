use super::*;

// --- publication ---

impl LexiconService {}

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
                let Some((target_entry_id, target_sense_id)) = relation.bound_target() else {
                    // Unlinked display text is valid in drafts and publications.
                    if let Some(issue) = pending_relation_issue(relation) {
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
    if record.content_schema_version != 3 {
        return Err(LexiconServiceError::UnsupportedSchemaVersion(
            record.content_schema_version,
        ));
    }
    record
        .presentation_label
        .clone()
        .ok_or_else(invariant_record)
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
        3 => {
            let word: AdminWordV3 =
                serde_json::from_value(record.snapshot.clone()).map_err(serialization_error)?;
            let meanings =
                crate::lexicon::sentence_association::v3_meanings_to_relational(&word.meanings)
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

/// 校验未绑定关联词的文本存储限制；保存和发布使用相同规则。
pub(super) fn pending_relation_issue(relation: &WordRelationV2) -> Option<DraftValidationIssue> {
    if relation.pending_target_headword.is_none()
        && relation.prebound_target_word_id.is_none()
        && relation.pending_target_gloss.is_some()
    {
        return Some(reference_issue(
            relation.id,
            "pending_target_gloss",
            "relation_pending_gloss_without_headword",
            "文本注释必须跟随关联词文本",
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
    if relation.prebound_target_word_id.is_some() {
        return Some(reference_issue(
            relation.id,
            "prebound_target_word_id",
            "relation_target_shape_invalid",
            "预绑定已停用，请选择具体词义或使用纯文本",
        ));
    }
    // 手输关联词是独立展示文本，保存和发布均保留，不创建或自动绑定词条。
    // 两个阶段只校验存储限制，不要求它是合法英文词条名。
    if relation
        .pending_target_headword
        .as_deref()
        .is_some_and(|value| {
            value.chars().any(char::is_control)
                || value.chars().count() > crate::lexicon::normalization::MAX_HEADWORD_CODEPOINTS
        })
    {
        return Some(reference_issue(
            relation.id,
            "pending_target_headword",
            "relation_pending_headword_invalid",
            "关联词文本不能含控制字符，且不超过 200 个字符",
        ));
    }
    None
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
