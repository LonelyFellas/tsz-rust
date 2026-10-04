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
    Ok(())
}

pub(crate) fn valid_ranges(variant: &GrammarVariantV3) -> bool {
    let links = variant.form_links.as_deref().unwrap_or_default();
    if links.len() > 100 {
        return false;
    }
    let chars: Vec<_> = variant.content.text().chars().collect();
    let tokens = crate::lexicon::text_tokenization::tokenize(variant.content.text());
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

pub(super) fn target_variant(
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
        WordRegionalVariantsV3::Common { common } => {
            Some((common.id, Dialect::Common, common.pronunciations.clone()))
        }
        WordRegionalVariantsV3::UkUs { uk, us } => {
            if uk.id == link.target_variant_id || link.target_dialect == Dialect::Uk {
                Some((uk.id, Dialect::Uk, uk.pronunciations.clone()))
            } else if us.id == link.target_variant_id || link.target_dialect == Dialect::Us {
                Some((us.id, Dialect::Us, us.pronunciations.clone()))
            } else {
                None
            }
        }
    }
}

fn first_annotation(
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
                    target_variant(&target.forms, link)
                };
            let (id, dialect, pronunciations) =
                resolved.ok_or_else(|| invalid(variant.id, "关联词形已失效，请重新关联"))?;
            link.target_variant_id = id;
            link.target_dialect = dialect;
            let segment = &link.source_segments[0];
            if let Some(annotation) = first_annotation(
                &pronunciations,
                segment.start,
                segment.end,
                variant.dialect,
                dialect,
            )
            .map_err(|message| invalid(variant.id, message))?
            {
                rich.annotations.push(annotation);
            }
        }
    }
    if !crate::lexicon::rich_text::canonicalize_meanings(content) {
        return Err(invalid(
            entry_id,
            "关联词形的第一个发音包含不可用于合成的音素，请检查发音配置",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

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
