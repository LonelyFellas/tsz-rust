use std::collections::{HashMap, HashSet};

use sqlx::{Postgres, Transaction};

use super::v3::{
    english_text_variants_mut, load_phrase_component_target, source_english_texts,
    target_english_texts,
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

fn target_gloss(target: &AdminWordV3, link: &TextLinkV3) -> Option<String> {
    let pos = target
        .forms
        .pos
        .iter()
        .find(|p| p.pos_id == link.target_pos_id)?;
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
    let sense = target
        .meanings
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

fn component_matches(phrase: &AdminWordV3, link: &TextLinkV3) -> bool {
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

/// 关联保存在正文变体的 V3 投影/发布快照，引用保护复用 publication_sense_refs。
pub(super) async fn validate_targets(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    content: &mut DraftMeaningsStepContentV3,
) -> Result<Vec<NewPublicationSenseReference>, LexiconServiceError> {
    let mut references = Vec::new();
    let mut seen_refs = HashMap::new();
    let mut targets = HashMap::new();
    let mut link_ids = HashSet::new();
    for variant in content
        .pos
        .iter_mut()
        .flat_map(target_english_texts)
        .flat_map(english_text_variants_mut)
    {
        if !valid_ranges(variant) {
            return Err(invalid(
                variant.id,
                "关联词段无效、重叠或超过上限，请重新选择",
            ));
        }
        for link in &mut variant.text_links {
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
            let mut requested = vec![(
                link.target_word_id,
                link.target_publication_id,
                link.target_sense_id,
            )];
            if let Some(via) = &link.via_phrase {
                requested.push((via.word_id, via.publication_id, via.sense_id));
            }
            for (word_id, publication_id, sense_id) in requested {
                let key = (word_id, publication_id);
                if let std::collections::hash_map::Entry::Vacant(entry) = targets.entry(key) {
                    let target = load_phrase_component_target(tx, word_id, publication_id)
                        .await?
                        .ok_or_else(|| invalid(variant.id, "关联目标已不可用，请重新选择"))?;
                    entry.insert(target);
                }
                let target = &targets[&key];
                let reference_key = (variant.id, word_id, sense_id);
                if let Some(previous) = seen_refs.insert(reference_key, publication_id) {
                    if previous != publication_id {
                        return Err(invalid(
                            variant.id,
                            "同一正文的同一目标词义须使用同一发布版本",
                        ));
                    }
                } else {
                    references.push(NewPublicationSenseReference {
                        source_node_id: variant.id,
                        reference_kind: PublicationSenseReferenceKind::TextLink,
                        target_entry_id: word_id,
                        target_sense_id: sense_id,
                        target_publication_id: Some(publication_id),
                        target_content_scope: PublicationTargetContentScope::Publication,
                        target_revision: target.revision,
                    });
                }
            }
            let target = &targets[&(link.target_word_id, link.target_publication_id)];
            let gloss = target_gloss(target, link).ok_or_else(|| {
                invalid(variant.id, "关联词形与词义不属于所选发布版本，请重新选择")
            })?;
            if let Some(via) = &link.via_phrase
                && !component_matches(&targets[&(via.word_id, via.publication_id)], link)
            {
                return Err(invalid(variant.id, "短语成分与关联目标不一致，请重新选择"));
            }
            link.target_headword = Some(target.presentation.label.clone());
            link.target_gloss = Some(gloss);
        }
    }
    let entries: Vec<_> = references.iter().map(|r| r.target_entry_id).collect();
    let publications: Vec<_> = references
        .iter()
        .map(|r| r.target_publication_id.unwrap())
        .collect();
    let senses: Vec<_> = references.iter().map(|r| r.target_sense_id).collect();
    let verified = LexiconRepository::phrase_component_publication_targets_for_publish(
        tx,
        &entries,
        &publications,
        &senses,
    )
    .await
    .map_err(repository_error)?;
    if verified.len() != references.len() {
        return Err(LexiconServiceError::ReferenceConflict);
    }
    let revisions: HashMap<_, _> = verified
        .into_iter()
        .map(|(entry, publication, sense, revision)| ((entry, publication, sense), revision))
        .collect();
    for reference in &mut references {
        reference.target_revision = revisions[&(
            reference.target_entry_id,
            reference.target_publication_id.unwrap(),
            reference.target_sense_id,
        )];
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
                        target_publication_id: Some(link.target_publication_id),
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
