use super::*;

pub(super) fn validate_slot_variants(
    issues: &mut Vec<DraftValidationIssue>,
    node_types: &mut HashMap<Uuid, &'static str>,
    slot_id: Uuid,
    variants: &[crate::lexicon::dto::WordFormVariantV2],
    spelling_mode: &str,
    phonetic_mode: &str,
    headwords: Option<&WordHeadwordsV2>,
) {
    let expected = if spelling_mode == "distinguish" || phonetic_mode == "distinguish" {
        vec![Dialect::Uk, Dialect::Us]
    } else {
        vec![Dialect::Common]
    };
    let actual = variants
        .iter()
        .map(|variant| variant.dialect)
        .collect::<HashSet<_>>();
    if actual.len() != variants.len() {
        issue(
            issues,
            PersistedWordStep::Forms,
            slot_id,
            "variants",
            "duplicate_dialect_variant",
            "同一词形不能重复添加相同方言行",
        );
    }
    if actual.len() != expected.len() || expected.iter().any(|value| !actual.contains(value)) {
        issue(
            issues,
            PersistedWordStep::Forms,
            slot_id,
            "variants",
            "dialect_variants_invalid",
            "词形方言行必须与当前方言规则完全一致",
        );
    }
    for variant in variants {
        unique_node(
            issues,
            node_types,
            PersistedWordStep::Forms,
            variant.id,
            "form_variant",
        );
        if variant.spelling.trim().is_empty() {
            issue(
                issues,
                PersistedWordStep::Forms,
                variant.id,
                "spelling",
                "spelling_required",
                "拼写不能为空",
            );
        }
        if variant.spelling.trim() != variant.spelling {
            issue(
                issues,
                PersistedWordStep::Forms,
                variant.id,
                "spelling",
                "spelling_not_trimmed",
                "拼写不能包含首尾空白",
            );
        }
        if variant.spelling.chars().count() > 200 {
            issue(
                issues,
                PersistedWordStep::Forms,
                variant.id,
                "spelling",
                "spelling_too_long",
                "拼写不能超过 200 个字符",
            );
        }
        if !variant.spelling.is_empty()
            && crate::lexicon::normalization::normalize_headword(&variant.spelling).is_err()
        {
            issue(
                issues,
                PersistedWordStep::Forms,
                variant.id,
                "spelling",
                "spelling_not_normalizable",
                "拼写包含不支持的字符或归一化后过长",
            );
        }
        if let Some(headwords) = headwords {
            let expected_spelling = match (headwords, variant.dialect) {
                (WordHeadwordsV2::Unified { common }, _) => Some(common.as_str()),
                (WordHeadwordsV2::Distinguish { uk, .. }, Dialect::Uk) => Some(uk.as_str()),
                (WordHeadwordsV2::Distinguish { us, .. }, Dialect::Us) => Some(us.as_str()),
                _ => None,
            };
            if expected_spelling != Some(variant.spelling.as_str()) {
                issue(
                    issues,
                    PersistedWordStep::Forms,
                    variant.id,
                    "spelling",
                    "base_spelling_mismatch",
                    "原形拼写必须与只读主词形一致",
                );
            }
        }
        if variant.pronunciations.is_empty()
            || variant.pronunciations.iter().any(|pronunciation| {
                pronunciation.dict_phonetic.trim().is_empty()
                    || pronunciation.actual_pron.trim().is_empty()
            })
        {
            issue(
                issues,
                PersistedWordStep::Forms,
                variant.id,
                "pronunciations",
                "pronunciation_required",
                "词典音标和实际发音不能为空",
            );
        }
        for pronunciation in &variant.pronunciations {
            unique_node(
                issues,
                node_types,
                PersistedWordStep::Forms,
                pronunciation.id,
                "pronunciation",
            );
            if pronunciation.dict_phonetic.chars().count() > 200 {
                issue(
                    issues,
                    PersistedWordStep::Forms,
                    pronunciation.id,
                    "dict_phonetic",
                    "dict_phonetic_too_long",
                    "词典音标不能超过 200 个字符",
                );
            }
            if pronunciation.actual_pron.chars().count() > 200 {
                issue(
                    issues,
                    PersistedWordStep::Forms,
                    pronunciation.id,
                    "actual_pron",
                    "actual_pron_too_long",
                    "实际发音不能超过 200 个字符",
                );
            }
        }
    }
}

pub(super) fn valid_english_text(value: &EnglishTextV2) -> bool {
    match value {
        EnglishTextV2::Unified { common } => {
            valid_rich_text(&common.value) && !common.value.text().trim().is_empty()
        }
        EnglishTextV2::Distinguish { uk, us, .. } => [uk, us].iter().all(|slot| {
            matches!(slot, DialectVariantSlotV2::Ready { variant }
                if valid_rich_text(&variant.value) && !variant.value.text().trim().is_empty())
        }),
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

pub(super) fn register_english_text_nodes(
    issues: &mut Vec<DraftValidationIssue>,
    node_types: &mut HashMap<Uuid, &'static str>,
    value: &EnglishTextV2,
) {
    match value {
        EnglishTextV2::Unified { common } => unique_node(
            issues,
            node_types,
            PersistedWordStep::Meanings,
            common.id,
            "text_variant",
        ),
        EnglishTextV2::Distinguish { uk, us, .. } => {
            for slot in [uk, us] {
                if let DialectVariantSlotV2::Ready { variant } = slot {
                    unique_node(
                        issues,
                        node_types,
                        PersistedWordStep::Meanings,
                        variant.id,
                        "text_variant",
                    );
                }
            }
        }
    }
}

pub(super) fn valid_rich_text(value: &RichText) -> bool {
    crate::lexicon::rich_text::is_valid(value)
}

pub(super) fn valid_percent(value: &str) -> bool {
    let Some((whole, decimal)) = value.split_once('.').map_or_else(
        || Some((value, None)),
        |(whole, decimal)| Some((whole, Some(decimal))),
    ) else {
        return false;
    };
    if whole.is_empty()
        || !whole.chars().all(|character| character.is_ascii_digit())
        || decimal.is_some_and(|decimal| {
            decimal.is_empty()
                || decimal.len() > 2
                || !decimal.chars().all(|character| character.is_ascii_digit())
        })
    {
        return false;
    }
    value
        .parse::<f64>()
        .is_ok_and(|number| (0.0..=100.0).contains(&number))
}

pub(super) fn valid_level(value: &str) -> bool {
    matches!(value, "A1" | "A2" | "B1" | "B2" | "C1" | "C2")
}

pub(super) fn valid_form_type(value: &str) -> bool {
    matches!(
        value,
        "present_participle"
            | "past_tense"
            | "past_participle"
            | "third_person_singular"
            | "plural"
            | "comparative"
            | "superlative"
    )
}

pub(super) fn unique_node(
    issues: &mut Vec<DraftValidationIssue>,
    nodes: &mut HashMap<Uuid, &'static str>,
    step: PersistedWordStep,
    id: Uuid,
    node_type: &'static str,
) {
    if let Some(previous) = nodes.insert(id, node_type) {
        issue(
            issues,
            step,
            id,
            "id",
            "node_id_reused",
            if previous == node_type {
                "节点 ID 在请求中重复"
            } else {
                "节点 ID 不能用于不同节点类型"
            },
        );
    }
}

pub(super) fn issue(
    issues: &mut Vec<DraftValidationIssue>,
    step: PersistedWordStep,
    node_id: Uuid,
    field: &str,
    code: &str,
    message: &str,
) {
    issues.push(DraftValidationIssue {
        step,
        node_id,
        field: field.to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        reference_location: None,
    });
}
