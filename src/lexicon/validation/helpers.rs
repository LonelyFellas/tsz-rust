use super::*;

pub(super) fn valid_english_text(value: &EnglishTextV3) -> bool {
    match value {
        EnglishTextV3::Unified { common } => {
            valid_rich_text(&common.value) && !common.value.text().trim().is_empty()
        }
        EnglishTextV3::Distinguish { uk, us, .. } => [uk, us].iter().all(|slot| {
            matches!(slot, DialectVariantRichTextSlotV3::Ready { variant }
                if valid_rich_text(&variant.value) && !variant.value.text().trim().is_empty())
        }),
    }
}

pub(super) fn definition_level(definition: &WordDefinitionV3) -> &str {
    match definition {
        WordDefinitionV3::ZhDefinition { level, .. }
        | WordDefinitionV3::ZhSentence { level, .. }
        | WordDefinitionV3::EnDefinition { level, .. }
        | WordDefinitionV3::EnSentence { level, .. } => level,
    }
}

pub(super) fn register_english_text_nodes(
    issues: &mut Vec<DraftValidationIssue>,
    node_types: &mut HashMap<Uuid, &'static str>,
    value: &EnglishTextV3,
) {
    match value {
        EnglishTextV3::Unified { common } => unique_node(
            issues,
            node_types,
            PersistedWordStep::Meanings,
            common.id,
            "text_variant",
        ),
        EnglishTextV3::Distinguish { uk, us, .. } => {
            for slot in [uk, us] {
                if let DialectVariantRichTextSlotV3::Ready { variant } = slot {
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

pub(super) fn valid_rich_text(value: &RichTextV3) -> bool {
    crate::lexicon::rich_text::is_valid_native(value)
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
        node_location: None,
    });
}
