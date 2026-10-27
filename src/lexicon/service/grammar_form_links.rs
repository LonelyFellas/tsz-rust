use super::*;
use crate::lexicon::dto::{
    GrammarFormLinkV3, GrammarVariantV3, RichTextAnnotationV3, RichTextPhonemeAlphabet, RichTextV3,
    WordPronunciationV3,
};
use sqlx::{Postgres, Transaction};
use std::collections::{HashMap, HashSet};

fn invalid(node_id: Uuid, message: &str) -> LexiconServiceError {
    v3_validation_failed(vec![DraftValidationIssue {
        step: PersistedWordStep::Meanings,
        node_id,
        field: "content".to_owned(),
        code: "grammar_form_link_invalid".to_owned(),
        message: message.to_owned(),
        reference_location: None,
        node_location: None,
    }])
}

pub(super) fn variants(
    content: &DraftMeaningsStepContentV3,
) -> impl Iterator<Item = &GrammarVariantV3> {
    content
        .pos
        .iter()
        .flat_map(|pos| &pos.grammar_structures)
        .flat_map(|structure| &structure.variants)
}

pub(super) fn preserve_missing(
    next: &mut DraftMeaningsStepContentV3,
    previous: &DraftMeaningsStepContentV3,
) -> Result<(), LexiconServiceError> {
    let old: HashMap<_, _> = variants(previous)
        .map(|variant| (variant.id, variant))
        .collect();
    for variant in next
        .pos
        .iter_mut()
        .flat_map(|pos| &mut pos.grammar_structures)
        .flat_map(|structure| &mut structure.variants)
    {
        if variant.form_links.is_none()
            && let Some(previous) = old.get(&variant.id)
        {
            if previous
                .form_links
                .as_ref()
                .is_some_and(|links| !links.is_empty())
                && previous.content.text() != variant.content.text()
            {
                return Err(invalid(
                    variant.id,
                    "正文已改变，请使用支持关联词形的编辑器重新确认关联",
                ));
            }
            variant.form_links = previous.form_links.clone();
        }
    }
    let old: HashMap<_, _> = super::text_links::variants(previous)
        .map(|variant| (variant.id, variant))
        .collect();
    for variant in next
        .pos
        .iter_mut()
        .flat_map(super::v3::target_english_texts)
        .flat_map(super::v3::english_text_variants_mut)
    {
        if let Some(previous) = old.get(&variant.id) {
            if !variant.reading_links.was_present()
                && !previous.reading_links.is_empty()
                && previous.value.text() != variant.value.text()
            {
                return Err(invalid(variant.id, "正文已改变，请重新确认读法关联"));
            }
            variant
                .reading_links
                .preserve_missing_from(&previous.reading_links);
        }
    }
    Ok(())
}

pub(crate) fn valid_ranges(variant: &GrammarVariantV3) -> bool {
    valid_reading_ranges(
        variant.content.text(),
        variant.form_links.as_deref().unwrap_or_default(),
    )
}

pub(crate) fn valid_reading_ranges(text: &str, links: &[GrammarFormLinkV3]) -> bool {
    if links.len() > 100 {
        return false;
    }
    let chars: Vec<_> = text.chars().collect();
    let tokens = crate::lexicon::text_tokenization::tokenize(text);
    let mut ids = HashSet::new();
    let mut occupied = HashSet::new();
    links.iter().all(|link| {
        if link.id.is_nil() || !ids.insert(link.id) || link.source_segments.len() != 1 {
            return false;
        }
        let segment = &link.source_segments[0];
        segment.start < segment.end
            && segment.end <= chars.len()
            && tokens
                .iter()
                .any(|token| token.range.start == segment.start && token.range.end == segment.end)
            && chars[segment.start..segment.end].iter().collect::<String>() == segment.surface
            && (segment.start..segment.end).all(|index| occupied.insert(index))
    })
}

pub(crate) fn target_variant(
    forms: &DraftFormsStepContentV3,
    link: &GrammarFormLinkV3,
) -> Option<(Uuid, Dialect, Vec<WordPronunciationV3>)> {
    let form = forms
        .pos
        .iter()
        .find(|pos| pos.pos_id == link.target_pos_id)?
        .forms
        .iter()
        .find(|form| form.id == link.target_form_id)?;
    match &form.regional_variants {
        WordRegionalVariantsV3::Common { common } if common.id == link.target_variant_id => {
            Some((common.id, Dialect::Common, common.pronunciations.clone()))
        }
        WordRegionalVariantsV3::Common { .. } => None,
        WordRegionalVariantsV3::UkUs { uk, us } => {
            if uk.id == link.target_variant_id {
                Some((uk.id, Dialect::Uk, uk.pronunciations.clone()))
            } else if us.id == link.target_variant_id {
                Some((us.id, Dialect::Us, us.pronunciations.clone()))
            } else {
                None
            }
        }
    }
}

pub(crate) fn reading_revision(
    synthesis: &crate::lexicon::dto::PronunciationSynthesisV3,
    locale: crate::lexicon::dto::PhonemeLocaleV3,
    legacy: Dialect,
    alphabet: RichTextPhonemeAlphabet,
) -> String {
    let selected = synthesis.for_locale(locale, legacy);
    let phone = match alphabet {
        RichTextPhonemeAlphabet::Ipa => &selected.ipa,
        RichTextPhonemeAlphabet::Ups => &selected.ups,
    };
    let words = if matches!(alphabet, RichTextPhonemeAlphabet::Ups) {
        selected.ups_words.map(|words| {
            words
                .into_iter()
                .map(|word| (word.text, word.phoneme))
                .collect::<Vec<_>>()
        })
    } else {
        None
    };
    serde_json::json!([
        locale,
        alphabet,
        phone,
        words,
        selected.use_spelling.unwrap_or(false)
    ])
    .to_string()
}

pub(crate) fn annotation_for_reading(
    pronunciations: &[WordPronunciationV3],
    start: usize,
    end: usize,
    dialect: Dialect,
    legacy: Dialect,
    reference: &crate::lexicon::dto::ReadingReferenceV3,
) -> Result<Option<RichTextAnnotationV3>, &'static str> {
    use crate::lexicon::dto::PhonemeLocaleV3;
    if dialect == Dialect::Common {
        if reference.uk.is_none() && reference.us.is_none() {
            return Err("请选择具体口音的读法");
        }
        for (dialect, selected) in [(Dialect::Uk, &reference.uk), (Dialect::Us, &reference.us)] {
            if selected.is_some() {
                annotation_for_reading(pronunciations, start, end, dialect, legacy, reference)?;
            }
        }
        return Ok(None);
    }
    let reference = if dialect == Dialect::Uk {
        reference.uk.as_ref()
    } else {
        reference.us.as_ref()
    }
    .ok_or("当前口音未选择读法，请明确选择")?;
    let row = pronunciations
        .iter()
        .find(|row| row.id == reference.pronunciation_id)
        .ok_or("指定读法已失效，请重新选择读法")?;
    let synthesis = row
        .synthesis
        .as_ref()
        .ok_or("指定读法未配置合成音标，请修复读法")?;
    if matches!(
        (dialect, reference.locale),
        (Dialect::Uk, PhonemeLocaleV3::EnUs) | (Dialect::Us, PhonemeLocaleV3::EnGb)
    ) {
        return Err("指定读法与当前口音不匹配");
    }
    let mut synthesis = synthesis.for_locale(reference.locale, legacy);
    synthesis.use_spelling = Some(reference.use_spelling);
    if reading_revision(&synthesis, reference.locale, legacy, reference.alphabet)
        != reference.revision
    {
        return Err("指定读法已变化，请重新确认读法版本");
    }
    let mut row = row.clone();
    synthesis.alphabet = reference.alphabet;
    row.synthesis = Some(synthesis);
    first_annotation(&[row], start, end, dialect, legacy)
}

pub(crate) fn first_annotation(
    pronunciations: &[WordPronunciationV3],
    start: usize,
    end: usize,
    dialect: Dialect,
    legacy_dialect: Dialect,
) -> Result<Option<RichTextAnnotationV3>, &'static str> {
    let synthesis = pronunciations
        .first()
        .and_then(|first| first.synthesis.as_ref())
        .ok_or("关联词形的第一个发音未配置，请先完善该发音")?;
    if synthesis.use_spelling == Some(true) {
        return Ok(None);
    }
    use crate::lexicon::dto::PhonemeLocaleV3;
    let dual = synthesis.uk.is_some() || synthesis.us.is_some();
    // 通用正文的双口音音素由试听的目标 locale 决定，不能冻结成某一侧。
    if dual && dialect == Dialect::Common {
        return Ok(None);
    }
    let selected = match dialect {
        Dialect::Uk => synthesis.for_locale(PhonemeLocaleV3::EnGb, legacy_dialect),
        Dialect::Us => synthesis.for_locale(PhonemeLocaleV3::EnUs, legacy_dialect),
        Dialect::Common => synthesis.clone(),
    };
    let synthesis = &selected;
    let recorded_locale = match synthesis.alphabet {
        RichTextPhonemeAlphabet::Ipa => synthesis.ipa_locale,
        RichTextPhonemeAlphabet::Ups => synthesis.ups_locale,
    };
    if matches!(
        (dialect, recorded_locale),
        (Dialect::Uk, Some(PhonemeLocaleV3::EnUs)) | (Dialect::Us, Some(PhonemeLocaleV3::EnGb))
    ) {
        return Err("关联词形的第一个发音与当前口音不匹配，请先完善该发音");
    }
    let phoneme = match synthesis.alphabet {
        RichTextPhonemeAlphabet::Ipa => &synthesis.ipa,
        RichTextPhonemeAlphabet::Ups => &synthesis.ups,
    };
    if phoneme.trim().is_empty() || phoneme.chars().any(char::is_control) {
        return Err("关联词形的第一个发音未配置，请先完善该发音");
    }
    Ok(Some(RichTextAnnotationV3::Phoneme {
        start,
        end,
        alphabet: synthesis.alphabet,
        phoneme: phoneme.trim().to_owned(),
    }))
}

pub(super) async fn validate_targets(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    kind: WordEntryKindV3,
    forms: &DraftFormsStepContentV3,
    content: &mut DraftMeaningsStepContentV3,
    batch: Option<&super::v3_publication::PublicationBatchContext>,
    publishing: bool,
) -> Result<(), LexiconServiceError> {
    let current_meanings = content.clone();
    for variant in content
        .pos
        .iter_mut()
        .flat_map(|pos| &mut pos.grammar_structures)
        .flat_map(|structure| &mut structure.variants)
    {
        if !valid_ranges(variant) {
            return Err(invalid(
                variant.id,
                "请以完整单词关联词形，不得重复或重叠关联",
            ));
        }
        let Some(links) = &mut variant.form_links else {
            continue;
        };
        let RichTextV3::V2(rich) = &mut variant.content else {
            if links.is_empty() {
                continue;
            }
            return Err(invalid(variant.id, "请重新打开语法结构编辑器确认词形关联"));
        };
        rich.annotations
            .retain(|annotation| !matches!(annotation, RichTextAnnotationV3::Phoneme { .. }));
        for link in links {
            let resolved =
                if link.target_word_id == entry_id && link.target_publication_id.is_none() {
                    if kind != WordEntryKindV3::Word {
                        return Err(invalid(variant.id, "关联词形只支持单词"));
                    }
                    if !reading_sense_valid(forms, &current_meanings, link) {
                        return Err(invalid(variant.id, "读法义项不属于当前词性或词形"));
                    }
                    target_variant(forms, link)
                } else {
                    let target = super::v3::resolve_component_target_in(
                        tx,
                        link.target_word_id,
                        link.target_publication_id,
                        batch,
                        // 草稿与发布版可能具有相同节点 ID、不同首条发音，不能自动切换范围。
                        |_| false,
                    )
                    .await?
                    .ok_or_else(|| invalid(variant.id, "关联词形目标已失效，请重新关联"))?;
                    if target.kind != WordEntryKindV3::Word {
                        return Err(invalid(variant.id, "关联词形只支持单词"));
                    }
                    if publishing && target.publication_id().is_none() {
                        return Err(invalid(variant.id, "请先发布目标单词，再关联其已发布词形"));
                    }
                    link.target_publication_id = target.publication_id();
                    if !reading_sense_valid(&target.forms, &target.meanings, link) {
                        return Err(invalid(variant.id, "读法义项不属于当前词性或词形"));
                    }
                    target_variant(&target.forms, link)
                };
            let (id, dialect, pronunciations) =
                resolved.ok_or_else(|| invalid(variant.id, "关联词形已失效，请重新关联"))?;
            link.target_variant_id = id;
            link.target_dialect = dialect;
            let segment = &link.source_segments[0];
            let annotation = if let Some(reference) = &link.reading_reference {
                annotation_for_reading(
                    &pronunciations,
                    segment.start,
                    segment.end,
                    variant.dialect,
                    dialect,
                    reference,
                )
            } else {
                first_annotation(
                    &pronunciations,
                    segment.start,
                    segment.end,
                    variant.dialect,
                    dialect,
                )
            }
            .map_err(|message| invalid(variant.id, message))?;
            if let Some(annotation) = annotation {
                rich.annotations.push(annotation);
            }
        }
    }
    let mut texts = content
        .pos
        .iter_mut()
        .flat_map(super::v3::target_english_texts)
        .collect::<Vec<_>>();
    for text in &mut texts {
        validate_reading_text(
            tx,
            text,
            Some((entry_id, forms, &current_meanings)),
            batch,
            publishing,
        )
        .await?;
    }
    if !crate::lexicon::rich_text::canonicalize_meanings(content) {
        return Err(invalid(
            entry_id,
            "关联词形的第一个发音包含不可用于合成的音素，请检查发音配置",
        ));
    }
    Ok(())
}

pub(crate) async fn validate_reading_text(
    tx: &mut Transaction<'_, Postgres>,
    text: &mut crate::lexicon::dto::EnglishTextV3,
    current: Option<(Uuid, &DraftFormsStepContentV3, &DraftMeaningsStepContentV3)>,
    batch: Option<&super::v3_publication::PublicationBatchContext>,
    publishing: bool,
) -> Result<(), LexiconServiceError> {
    for variant in super::v3::english_text_variants_mut(text) {
        if !valid_reading_ranges(variant.value.text(), &variant.reading_links) {
            return Err(invalid(variant.id, "读法关联必须绑定完整单词且不能重叠"));
        }
        for link in variant.reading_links.iter_mut() {
            let local = current.filter(|(id, _, _)| {
                *id == link.target_word_id && link.target_publication_id.is_none()
            });
            let resolved;
            let (forms, meanings) = if let Some((_, forms, meanings)) = local {
                (forms, meanings)
            } else {
                resolved = super::v3::resolve_component_target_in(
                    tx,
                    link.target_word_id,
                    link.target_publication_id,
                    batch,
                    |_| false,
                )
                .await?
                .ok_or_else(|| invalid(variant.id, "读法目标已失效"))?;
                if resolved.kind != WordEntryKindV3::Word
                    || publishing && resolved.publication_id().is_none()
                {
                    return Err(invalid(variant.id, "请先发布读法目标单词"));
                }
                link.target_publication_id = resolved.publication_id();
                (&resolved.forms, &resolved.meanings)
            };
            let (_, dialect, pronunciations) =
                target_variant(forms, link).ok_or_else(|| invalid(variant.id, "读法词形已失效"))?;
            let reference = link
                .reading_reference
                .as_ref()
                .ok_or_else(|| invalid(variant.id, "请选择具体读法"))?;
            if !reading_sense_valid(forms, meanings, link) {
                return Err(invalid(variant.id, "读法义项不属于当前词性或词形"));
            }
            let range = &link.source_segments[0];
            annotation_for_reading(
                &pronunciations,
                range.start,
                range.end,
                Dialect::Common,
                dialect,
                reference,
            )
            .map_err(|message| invalid(variant.id, message))?;
        }
    }
    Ok(())
}

pub(crate) fn reading_sense_valid(
    forms: &DraftFormsStepContentV3,
    meanings: &DraftMeaningsStepContentV3,
    link: &GrammarFormLinkV3,
) -> bool {
    let Some(sense_id) = link
        .reading_reference
        .as_ref()
        .and_then(|reference| reference.sense_id)
    else {
        return true;
    };
    forms
        .pos
        .iter()
        .find(|pos| pos.pos_id == link.target_pos_id)
        .is_some_and(|pos| {
            super::form_senses::allowed_form_senses(pos, meanings, link.target_form_id)
                .any(|sense| sense.id == sense_id)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn explicit_second_reading_and_field_are_pinned_and_stale_or_cross_accent_references_fail() {
        use crate::lexicon::dto::{PhonemeLocaleV3, ReadingReferenceV3, ReadingSelectionV3};
        let row = |phone: &str| {
            serde_json::from_value::<WordPronunciationV3>(json!({"id":Uuid::new_v4(),"dict_phonetic":"","actual_pron":"","synthesis":{"alphabet":"ipa","use_spelling":false,"ipa":phone,"ups":"K AE T","ipa_locale":"en-GB","ups_locale":"en-GB"}})).unwrap()
        };
        let rows = vec![row("kæt"), row("kɛt")];
        let selection = ReadingSelectionV3 {
            pronunciation_id: rows[1].id,
            alphabet: RichTextPhonemeAlphabet::Ipa,
            use_spelling: false,
            locale: PhonemeLocaleV3::EnGb,
            revision: reading_revision(
                rows[1].synthesis.as_ref().unwrap(),
                PhonemeLocaleV3::EnGb,
                Dialect::Uk,
                RichTextPhonemeAlphabet::Ipa,
            ),
        };
        let mut reference = ReadingReferenceV3 {
            uk: Some(selection),
            us: None,
            sense_id: None,
        };
        let resolved = annotation_for_reading(&rows, 0, 3, Dialect::Uk, Dialect::Uk, &reference)
            .unwrap()
            .unwrap();
        assert_eq!(serde_json::to_value(resolved).unwrap()["phoneme"], "kɛt");
        assert!(annotation_for_reading(&rows, 0, 3, Dialect::Us, Dialect::Uk, &reference).is_err());
        reference.uk.as_mut().unwrap().revision = "stale".into();
        assert!(annotation_for_reading(&rows, 0, 3, Dialect::Uk, Dialect::Uk, &reference).is_err());
    }

    #[test]
    fn grammar_form_ranges_require_single_whole_nonoverlapping_words() {
        let id = Uuid::new_v4();
        let link = json!({"id": id, "source_segments": [{"start": 4, "end": 7, "surface": "job"}], "target_word_id": id, "target_pos_id": id, "target_form_id": id, "target_variant_id": id, "target_dialect": "common"});
        let mut value = json!({"id": id, "dialect": "common", "content": {"version": 2, "text": "😀 a job", "annotations": []}, "form_links": [link.clone()]});
        let valid =
            |value| valid_ranges(&serde_json::from_value::<GrammarVariantV3>(value).unwrap());
        assert!(valid(value.clone()));
        value["form_links"][0]["source_segments"][0] =
            json!({"start": 5, "end": 7, "surface": "ob"});
        assert!(!valid(value.clone()));
        value["form_links"] = json!([link.clone(), link]);
        value["form_links"][1]["id"] = json!(Uuid::new_v4());
        assert!(!valid(value));
        assert!(matches!(
            invalid(id, "首条不可用"),
            LexiconServiceError::ValidationFailedV3(_)
        ));
    }

    #[test]
    fn grammar_form_dual_accent_does_not_freeze_common_text_to_one_accent() {
        let row: WordPronunciationV3 = serde_json::from_value(
            json!({"id":Uuid::new_v4(), "dict_phonetic":"", "actual_pron":"",
            "synthesis":{"alphabet":"ipa", "use_spelling":false, "ipa":"", "ups":"",
                "uk":{"ipa":"fɑː", "ups":""}, "us":{"ipa":"fɑɹ", "ups":""}}}),
        )
        .unwrap();
        assert!(
            first_annotation(
                std::slice::from_ref(&row),
                0,
                3,
                Dialect::Common,
                Dialect::Common
            )
            .unwrap()
            .is_none()
        );
        for (dialect, phoneme) in [(Dialect::Uk, "fɑː"), (Dialect::Us, "fɑɹ")] {
            let annotation =
                first_annotation(std::slice::from_ref(&row), 0, 3, dialect, Dialect::Common)
                    .unwrap()
                    .unwrap();
            assert_eq!(
                serde_json::to_value(annotation).unwrap()["phoneme"],
                phoneme
            );
        }
        let mut incomplete = row;
        incomplete
            .synthesis
            .as_mut()
            .unwrap()
            .us
            .as_mut()
            .unwrap()
            .ipa
            .clear();
        assert!(first_annotation(&[incomplete], 0, 3, Dialect::Us, Dialect::Common).is_err());
    }

    #[test]
    fn manual_source_selection_keeps_grammar_annotations_consistent() {
        for (alphabet, use_spelling) in [("ipa", false), ("ups", false), ("ipa", true)] {
            let row: WordPronunciationV3 = serde_json::from_value(json!({
                "id":Uuid::new_v4(), "dict_phonetic":"", "actual_pron":"",
                "synthesis":{"alphabet":alphabet, "use_spelling":use_spelling, "ipa":"", "ups":"",
                    "us":{"ipa":"fɑɹ", "ups":"F AA R"}}
            }))
            .unwrap();
            let annotation = first_annotation(&[row], 0, 3, Dialect::Us, Dialect::Common).unwrap();
            if use_spelling {
                assert!(annotation.is_none());
            } else {
                let value = serde_json::to_value(annotation.unwrap()).unwrap();
                assert_eq!(value["alphabet"], alphabet);
                assert_eq!(
                    value["phoneme"],
                    if alphabet == "ipa" { "fɑɹ" } else { "F AA R" }
                );
            }
        }
    }

    #[test]
    fn grammar_form_does_not_borrow_unlabelled_legacy_uk_phonemes_for_us() {
        let row: WordPronunciationV3 = serde_json::from_value(
            json!({"id":Uuid::new_v4(), "dict_phonetic":"", "actual_pron":"",
            "synthesis":{"alphabet":"ipa", "ipa":"fɑː", "ups":""}}),
        )
        .unwrap();
        assert!(first_annotation(&[row], 0, 3, Dialect::Us, Dialect::Uk).is_err());
    }

    #[test]
    fn grammar_form_first_pronunciation_only() {
        let mut first = json!({"id": Uuid::new_v4(), "dict_phonetic": "", "actual_pron": "", "synthesis": {"alphabet": "ipa", "ipa": "dʒɒb", "ups": ""}});
        let mut second = first.clone();
        second["synthesis"]["ipa"] = json!("never-read");
        let parse = |value| serde_json::from_value::<WordPronunciationV3>(value).unwrap();
        let annotation = first_annotation(
            &[parse(first.clone()), parse(second.clone())],
            2,
            5,
            Dialect::Common,
            Dialect::Common,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            serde_json::to_value(annotation).unwrap(),
            json!({"type": "phoneme", "start": 2, "end": 5, "alphabet": "ipa", "phoneme": "dʒɒb"})
        );
        first.as_object_mut().unwrap().remove("synthesis");
        assert!(
            first_annotation(
                &[parse(first), parse(second.clone())],
                2,
                5,
                Dialect::Common,
                Dialect::Common
            )
            .is_err()
        );
        assert!(first_annotation(&[], 2, 5, Dialect::Common, Dialect::Common).is_err());
        second["synthesis"]["use_spelling"] = json!(true);
        assert!(
            first_annotation(&[parse(second)], 2, 5, Dialect::Common, Dialect::Common)
                .unwrap()
                .is_none()
        );
    }
}
