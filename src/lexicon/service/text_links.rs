use std::collections::{BTreeMap, HashMap, HashSet};

use sqlx::{Postgres, Transaction};

use super::v3::{
    ComponentTargetScope, ComponentTargetWord, english_text_variants_mut, resolve_component_target,
    source_english_texts, target_english_texts,
};
use super::*;
use crate::lexicon::dto::{
    DraftMeaningsStepContentV3, PhraseComponentUsageV3, RichTextVariantV3, TextLinkV3,
    WordDefinitionV3,
};
use crate::lexicon::v3_contract::english_text_variants;

fn variants(content: &DraftMeaningsStepContentV3) -> impl Iterator<Item = &RichTextVariantV3> {
    content
        .pos
        .iter()
        .flat_map(source_english_texts)
        .flat_map(english_text_variants)
}

fn invalid(node_id: Uuid, message: &str) -> LexiconServiceError {
    v3_validation_failed(vec![DraftValidationIssue {
        step: PersistedWordStep::Meanings,
        node_id,
        field: "text_links".to_owned(),
        code: "definition_invalid".to_owned(),
        message: message.to_owned(),
        reference_location: None,
        node_location: None,
    }])
}

/// 旧客户端只有在正文未变时可以省略关联，不能把未知字段静默覆盖掉。
pub(super) fn preserve_missing(
    next: &mut DraftMeaningsStepContentV3,
    previous: &DraftMeaningsStepContentV3,
) -> Result<(), LexiconServiceError> {
    let old: HashMap<_, _> = variants(previous).map(|v| (v.id, v)).collect();
    for variant in next
        .pos
        .iter_mut()
        .flat_map(target_english_texts)
        .flat_map(english_text_variants_mut)
    {
        if variant.text_links.was_present() {
            continue;
        }
        if let Some(previous) = old.get(&variant.id) {
            if !previous.text_links.is_empty() && previous.value.text() != variant.value.text() {
                return Err(invalid(
                    variant.id,
                    "正文已改变，请使用支持关联的编辑器重新确认关联",
                ));
            }
            variant
                .text_links
                .preserve_missing_from(&previous.text_links);
        }
    }
    Ok(())
}

pub(super) fn restore(
    source: &DraftMeaningsStepContentV3,
    target: &mut DraftMeaningsStepContentV3,
) {
    let links: HashMap<_, _> = variants(source)
        .map(|v| (v.id, v.text_links.clone()))
        .collect();
    for variant in target
        .pos
        .iter_mut()
        .flat_map(target_english_texts)
        .flat_map(english_text_variants_mut)
    {
        variant.text_links = links.get(&variant.id).cloned().unwrap_or_default();
    }
}

pub(crate) fn valid_ranges(variant: &RichTextVariantV3) -> bool {
    if variant.text_links.len() > 100 {
        return false;
    }
    let chars: Vec<_> = variant.value.text().chars().collect();
    let tokens = crate::lexicon::sentence_target_discovery::tokenize(variant.value.text());
    let mut occupied = HashSet::new();
    let mut ids = HashSet::new();
    for link in &variant.text_links {
        if link.id.is_nil()
            || !ids.insert(link.id)
            || link.source_segments.is_empty()
            || link.source_segments.len() > 20
        {
            return false;
        }
        let mut previous_end = 0;
        for segment in &link.source_segments {
            let start = segment.start;
            let end = segment.end;
            if start >= end
                || start < previous_end
                || end > chars.len()
                || segment.surface.trim().is_empty()
                || !tokens.iter().any(|token| token.range.start == start)
                || !tokens.iter().any(|token| token.range.end == end)
                || chars[start..end].iter().collect::<String>() != segment.surface
                || chars[start..end].iter().any(|c| matches!(c, '\n' | '\r'))
                || (start..end).any(|i| !occupied.insert(i))
            {
                return false;
            }
            previous_end = end;
        }
        // 可分离短语也不能跨段落。
        let first = link.source_segments.first().unwrap().start;
        let last = link.source_segments.last().unwrap().end;
        if chars[first..last].iter().any(|c| matches!(c, '\n' | '\r')) {
            return false;
        }
    }
    true
}

fn target_gloss(target: &ComponentTargetWord, link: &TextLinkV3) -> Option<String> {
    target_content_gloss(&target.forms, &target.meanings, link)
}

fn target_content_gloss(
    forms: &DraftFormsStepContentV3,
    meanings: &DraftMeaningsStepContentV3,
    link: &TextLinkV3,
) -> Option<String> {
    let pos = forms.pos.iter().find(|p| p.pos_id == link.target_pos_id)?;
    let form = pos.forms.iter().find(|f| f.id == link.target_form_id)?;
    let valid_variant = match &form.regional_variants {
        WordRegionalVariantsV3::Common { common } => common.id == link.target_variant_id,
        WordRegionalVariantsV3::UkUs { uk, us } => {
            uk.id == link.target_variant_id || us.id == link.target_variant_id
        }
    };
    if !valid_variant
        || !pos
            .forms
            .iter()
            .any(|f| f.id == link.target_base_form_id && f.form_type == "base")
    {
        return None;
    }
    if link.target_form_id != link.target_base_form_id
        && !pos.form_groups.iter().any(|g| {
            g.members.iter().any(|m| m.form_id == link.target_form_id)
                && g.members
                    .iter()
                    .any(|m| m.form_id == link.target_base_form_id)
        })
    {
        return None;
    }
    let sense = meanings
        .pos
        .iter()
        .find(|p| p.pos_id == link.target_pos_id)?
        .senses
        .iter()
        .find(|s| s.id == link.target_sense_id)?;
    Some(
        sense
            .definitions
            .iter()
            .find_map(|d| match d {
                WordDefinitionV3::ZhDefinition { content, .. }
                | WordDefinitionV3::ZhSentence { content, .. } => Some(content.text().to_owned()),
                _ => None,
            })
            .unwrap_or_default(),
    )
}

fn component_matches(phrase: &ComponentTargetWord, link: &TextLinkV3) -> bool {
    let Some(via) = &link.via_phrase else {
        return true;
    };
    if phrase.kind != WordEntryKindV3::Phrase {
        return false;
    }
    phrase
        .meanings
        .pos
        .iter()
        .flat_map(|p| &p.senses)
        .find(|s| s.id == via.sense_id)
        .is_some_and(|sense| {
            sense.component_usages.iter().any(|usage| match usage {
                PhraseComponentUsageV3::Resolved {
                    id, target_word_id, ..
                } => *id == via.component_id && *target_word_id == link.target_word_id,
                _ => false,
            })
        })
}

/// 同一目标（词条 + 发布版本）上的全部链接：升级判定要看整组，一组只取一次目标。
#[derive(Default)]
struct TargetGroup {
    /// 报错时锚定的正文变体：取组里第一条链接所在的变体。
    variant_id: Uuid,
    /// 直接指向该目标的链接。
    direct: Vec<TextLinkV3>,
    /// 经该短语成分转关联的链接（目标是短语）。
    via: Vec<TextLinkV3>,
}

/// 关联保存在正文变体的 V3 投影/发布快照，引用保护复用 publication_sense_refs。
/// 目标可以是发布快照，也可以是从未发布的草稿（`target_publication_id` 缺省）：草稿目标发布后
/// 在这里升级并回填发布版本；仍是草稿的记 `draft` 范围引用，与关系词的稳定锚点同款。
pub(super) async fn validate_targets(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    content: &mut DraftMeaningsStepContentV3,
) -> Result<Vec<NewPublicationSenseReference>, LexiconServiceError> {
    let mut link_ids = HashSet::new();
    let mut groups = BTreeMap::<(Uuid, Option<Uuid>), TargetGroup>::new();
    for variant in content
        .pos
        .iter()
        .flat_map(source_english_texts)
        .flat_map(english_text_variants)
    {
        if !valid_ranges(variant) {
            return Err(invalid(
                variant.id,
                "关联词段无效、重叠或超过上限，请重新选择",
            ));
        }
        for link in &variant.text_links {
            if !link_ids.insert(link.id) {
                return Err(invalid(variant.id, "关联标识重复，请重新选择"));
            }
            if link.target_word_id == entry_id
                || link
                    .via_phrase
                    .as_ref()
                    .is_some_and(|via| via.word_id == entry_id)
            {
                return Err(invalid(variant.id, "不能关联当前正在编辑的词条"));
            }
            let group = groups
                .entry((link.target_word_id, link.target_publication_id))
                .or_insert_with(|| TargetGroup {
                    variant_id: variant.id,
                    ..TargetGroup::default()
                });
            group.direct.push(link.clone());
            if let Some(via) = &link.via_phrase {
                groups
                    .entry((via.word_id, via.publication_id))
                    .or_insert_with(|| TargetGroup {
                        variant_id: variant.id,
                        ..TargetGroup::default()
                    })
                    .via
                    .push(link.clone());
            }
        }
    }
    let mut targets = HashMap::<(Uuid, Option<Uuid>), ComponentTargetWord>::new();
    for (key, group) in &groups {
        let target = resolve_component_target(tx, key.0, key.1, |candidate| {
            group
                .direct
                .iter()
                .all(|link| target_gloss(candidate, link).is_some())
                && group
                    .via
                    .iter()
                    .all(|link| component_matches(candidate, link))
        })
        .await?
        .ok_or_else(|| invalid(group.variant_id, "关联目标已不可用，请重新选择"))?;
        targets.insert(*key, target);
    }

    let mut references = Vec::new();
    let mut seen_refs = HashMap::new();
    for variant in content
        .pos
        .iter_mut()
        .flat_map(target_english_texts)
        .flat_map(english_text_variants_mut)
    {
        for link in &mut variant.text_links {
            let target = &targets[&(link.target_word_id, link.target_publication_id)];
            // 草稿目标已发布：回填发布版本，之后与发布目标无异。
            if link.target_publication_id.is_none() {
                link.target_publication_id = target.publication_id();
            }
            let mut requested = vec![(link.target_word_id, target, link.target_sense_id)];
            let mut phrase = None;
            if let Some(via) = &mut link.via_phrase {
                let phrase_target = &targets[&(via.word_id, via.publication_id)];
                if via.publication_id.is_none() {
                    via.publication_id = phrase_target.publication_id();
                }
                requested.push((via.word_id, phrase_target, via.sense_id));
                phrase = Some(phrase_target);
            }
            for (word_id, target, sense_id) in requested {
                let publication_id = target.publication_id();
                let reference_key = (variant.id, word_id, sense_id);
                if let Some(previous) = seen_refs.insert(reference_key, publication_id) {
                    if previous != publication_id {
                        return Err(invalid(
                            variant.id,
                            "同一正文的同一目标词义须使用同一发布版本",
                        ));
                    }
                } else {
                    let (target_content_scope, target_revision) = match target.scope {
                        ComponentTargetScope::Publication { revision, .. } => {
                            (PublicationTargetContentScope::Publication, revision)
                        }
                        ComponentTargetScope::Draft { revision } => {
                            (PublicationTargetContentScope::Draft, revision)
                        }
                    };
                    references.push(NewPublicationSenseReference {
                        source_node_id: variant.id,
                        reference_kind: PublicationSenseReferenceKind::TextLink,
                        target_entry_id: word_id,
                        target_sense_id: sense_id,
                        target_publication_id: publication_id,
                        target_content_scope,
                        target_revision,
                    });
                }
            }
            let gloss = target_gloss(target, link).ok_or_else(|| {
                invalid(variant.id, "关联的词形或词义已不在目标词条里，请重新选择")
            })?;
            if let Some(phrase) = phrase
                && !component_matches(phrase, link)
            {
                return Err(invalid(variant.id, "短语成分与关联目标不一致，请重新选择"));
            }
            link.target_headword = Some(target.label.clone());
            link.target_gloss = Some(gloss);
        }
    }

    // 发布范围：锁那一版发布并取 source_revision；草稿范围：锁目标词条行并取 entry revision。
    let published = references
        .iter()
        .filter_map(|reference| {
            reference.target_publication_id.map(|publication_id| {
                (
                    reference.target_entry_id,
                    publication_id,
                    reference.target_sense_id,
                )
            })
        })
        .collect::<Vec<_>>();
    let entries = published
        .iter()
        .map(|(entry, _, _)| *entry)
        .collect::<Vec<_>>();
    let publications = published
        .iter()
        .map(|(_, publication, _)| *publication)
        .collect::<Vec<_>>();
    let senses = published
        .iter()
        .map(|(_, _, sense)| *sense)
        .collect::<Vec<_>>();
    let verified = LexiconRepository::phrase_component_publication_targets_for_publish(
        tx,
        &entries,
        &publications,
        &senses,
    )
    .await
    .map_err(repository_error)?;
    if verified.len() != published.len() {
        return Err(LexiconServiceError::ReferenceConflict);
    }
    let revisions: HashMap<_, _> = verified
        .into_iter()
        .map(|(entry, publication, sense, revision)| ((entry, Some(publication), sense), revision))
        .collect();
    let draft_keys = references
        .iter()
        .filter(|reference| reference.target_publication_id.is_none())
        .map(|reference| crate::lexicon::model::SenseTargetKey {
            target_entry_id: reference.target_entry_id,
            target_sense_id: reference.target_sense_id,
        })
        .collect::<Vec<_>>();
    let draft_revisions = LexiconRepository::draft_sense_targets_for_publish(tx, &draft_keys)
        .await
        .map_err(repository_error)?
        .into_iter()
        .filter(|record| !record.target_archived && !record.target_removed)
        .map(|record| {
            (
                (record.target_entry_id, None, record.target_sense_id),
                record.target_revision,
            )
        })
        .collect::<HashMap<_, _>>();
    for reference in &mut references {
        let key = (
            reference.target_entry_id,
            reference.target_publication_id,
            reference.target_sense_id,
        );
        reference.target_revision = revisions
            .get(&key)
            .or_else(|| draft_revisions.get(&key))
            .copied()
            .ok_or_else(|| invalid(reference.source_node_id, "关联目标已不可用，请重新选择"))?;
    }
    Ok(references)
}

/// 只组合输出，不把人工选择伪装成可写的旧 associations DTO。
pub(super) fn apply_manual(meanings: &mut DraftMeaningsStepContentV3) {
    use crate::lexicon::dto::{EnglishTextV3, WordSentenceAssociationV3};
    for sentence in meanings
        .pos
        .iter_mut()
        .flat_map(|p| &mut p.senses)
        .flat_map(|s| &mut s.sentences)
    {
        let variants: Vec<_> = match &sentence.en_text {
            EnglishTextV3::Unified { common } => vec![(Dialect::Common, common)],
            EnglishTextV3::Distinguish { uk, us, .. } => [(Dialect::Uk, uk), (Dialect::Us, us)]
                .into_iter()
                .filter_map(|(d, slot)| match slot {
                    crate::lexicon::dto::DialectVariantRichTextSlotV3::Ready { variant } => {
                        Some((d, variant))
                    }
                    _ => None,
                })
                .collect(),
        };
        for (dialect, variant) in variants {
            for link in &variant.text_links {
                sentence.associations.retain(|association| {
                    let (source_dialect, segments) = match association {
                        WordSentenceAssociationV3::Linked {
                            source_dialect,
                            source_segments,
                            ..
                        }
                        | WordSentenceAssociationV3::Pending {
                            source_dialect,
                            source_segments,
                            ..
                        } => (source_dialect, source_segments),
                    };
                    *source_dialect != dialect
                        || !segments.iter().any(|a| {
                            link.source_segments
                                .iter()
                                .any(|b| a.start < b.end && b.start < a.end)
                        })
                });
                sentence
                    .associations
                    .push(WordSentenceAssociationV3::Linked {
                        id: link.id,
                        association_schema_version: 3,
                        source_dialect: dialect,
                        source_segments: link.source_segments.clone(),
                        target_word_id: link.target_word_id,
                        target_sense_id: link.target_sense_id,
                        target_form_slot_id: Some(link.target_form_id),
                        target_publication_id: link.target_publication_id,
                        target_form_variant_id: Some(link.target_variant_id),
                        target_component_usages: vec![],
                        origin: SentenceAssociationOriginV2::Manual,
                        target_headword: link.target_headword.clone().unwrap_or_default(),
                        target_gloss: link.target_gloss.clone().unwrap_or_default(),
                        resolved_pos: String::new(),
                        resolved_form_type: None,
                    });
            }
        }
    }
}

fn shared_target_matches(
    forms: &DraftFormsStepContentV3,
    meanings: &DraftMeaningsStepContentV3,
    link: &TextLinkV3,
    dialect: &str,
) -> bool {
    if target_content_gloss(forms, meanings, link).is_none() {
        return false;
    }
    let Some(form) = forms
        .pos
        .iter()
        .find(|p| p.pos_id == link.target_pos_id)
        .and_then(|p| p.forms.iter().find(|f| f.id == link.target_form_id))
    else {
        return false;
    };
    let literal = link
        .source_segments
        .iter()
        .map(|s| s.surface.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let Ok(normalized) = normalize_headword(&literal) else {
        return false;
    };
    let matches = |id, spelling: &str, side: &str| {
        id == link.target_variant_id
            && (dialect == "common" || side == "common" || side == dialect)
            && normalize_headword(spelling).is_ok_and(|v| v.key == normalized.key)
    };
    match &form.regional_variants {
        WordRegionalVariantsV3::Common { common } => matches(common.id, &common.spelling, "common"),
        WordRegionalVariantsV3::UkUs { uk, us } => {
            matches(uk.id, &uk.spelling, "uk") || matches(us.id, &us.spelling, "us")
        }
    }
}

pub(crate) async fn validate_shared_sentence_target(
    tx: &mut Transaction<'_, Postgres>,
    link: &TextLinkV3,
    dialect: &str,
) -> Result<bool, LexiconServiceError> {
    // 共享例句从当前已保存词义反查；历史快照不能复活已删除的词义或词形。
    let Some((forms, meanings)) = sqlx::query_as::<_, (serde_json::Value, serde_json::Value)>(
        "SELECT forms,meanings FROM lexicon.entry_editor_projection WHERE entry_id=$1",
    )
    .bind(link.target_word_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    else {
        return Ok(false);
    };
    let forms = serde_json::from_value(forms).map_err(serialization_error)?;
    let meanings = serde_json::from_value(meanings).map_err(serialization_error)?;
    if !shared_target_matches(&forms, &meanings, link, dialect) {
        return Ok(false);
    }
    let target = resolve_component_target(
        tx,
        link.target_word_id,
        link.target_publication_id,
        |candidate| shared_target_matches(&candidate.forms, &candidate.meanings, link, dialect),
    )
    .await?;
    Ok(target.is_some_and(|target| {
        shared_target_matches(&target.forms, &target.meanings, link, dialect)
    }))
}

pub(crate) async fn ensure_shared_sentence_targets(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    forms: &DraftFormsStepContentV3,
    meanings: &DraftMeaningsStepContentV3,
) -> Result<(), LexiconServiceError> {
    use sqlx::Row;
    let refs=sqlx::query("SELECT a.id,a.target_ref,a.source_segments,a.source_dialect FROM lexicon.shared_sentence_annotations a JOIN lexicon.shared_sentences s ON s.id=a.sentence_id WHERE s.deleted_at IS NULL AND a.target_entry_id=$1 AND a.target_ref IS NOT NULL")
        .bind(entry_id).fetch_all(&mut **tx).await.map_err(database_error)?;
    for row in refs {
        let target: crate::lexicon::shared_sentences::SentenceTarget =
            serde_json::from_value(row.get("target_ref")).map_err(serialization_error)?;
        let segments =
            serde_json::from_value(row.get("source_segments")).map_err(serialization_error)?;
        let Some(link) = target.as_text_link(row.get("id"), segments) else {
            return Err(LexiconServiceError::SharedSentenceTargetInUse);
        };
        if !shared_target_matches(
            forms,
            meanings,
            &link,
            &row.get::<String, _>("source_dialect"),
        ) {
            return Err(LexiconServiceError::SharedSentenceTargetInUse);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn link(segments: Value) -> Value {
        json!({"id": Uuid::now_v7(), "source_segments": segments,
            "target_word_id": Uuid::now_v7(), "target_publication_id": Uuid::now_v7(), "target_pos_id": Uuid::now_v7(),
            "target_base_form_id": Uuid::now_v7(), "target_form_id": Uuid::now_v7(), "target_variant_id": Uuid::now_v7(), "target_sense_id": Uuid::now_v7()})
    }

    fn variant(text: &str, links: Value) -> RichTextVariantV3 {
        serde_json::from_value(json!({"id":Uuid::now_v7(),"origin":"manual","value":{"version":2,"text":text,"annotations":[]},"text_links":links})).unwrap()
    }

    #[test]
    fn text_links_validate_codepoints_segments_and_overlap() {
        let text = "👩 dress me up";
        let segments =
            json!([{"start":2,"end":7,"surface":"dress"},{"start":11,"end":13,"surface":"up"}]);
        let valid = link(segments.clone());
        assert!(valid_ranges(&variant(text, json!([valid.clone()]))));
        assert!(!valid_ranges(&variant(
            text,
            json!([valid, link(segments)])
        )));
        assert!(!valid_ranges(&variant(
            text,
            json!([link(json!([{"start":0,"end":7,"surface":"dress"}]))])
        )));
        assert!(!valid_ranges(&variant(
            text,
            json!([link(json!([{"start":2,"end":99,"surface":"dress"}]))])
        )));
        assert!(!valid_ranges(&variant(
            "mother",
            json!([link(json!([{"start":0,"end":3,"surface":"mot"}]))])
        )));
        assert!(!valid_ranges(&variant(
            "dress\nup",
            json!([link(
                json!([{"start":0,"end":5,"surface":"dress"},{"start":6,"end":8,"surface":"up"}])
            )])
        )));
    }

    #[test]
    fn text_links_omission_differs_from_explicit_empty() {
        let mut raw = serde_json::to_value(variant("mother", json!([]))).unwrap();
        let missing: RichTextVariantV3 = serde_json::from_value(raw.clone()).unwrap();
        assert!(!missing.text_links.was_present());
        raw["text_links"] = json!([]);
        let cleared: RichTextVariantV3 = serde_json::from_value(raw).unwrap();
        assert!(cleared.text_links.was_present());
    }
}
