use super::*;
use crate::lexicon::node_identity::{
    BASE_FORM_ROLE, FORM_GROUP_ROLE, GRAMMAR_STRUCTURE_ROLE, LEGACY_NODE_ROLE, POS_ROLE,
    PRONUNCIATION_ROLE, RELATION_ROLE, SENSE_GROUP_ROLE, SENSE_ROLE, SENTENCE_ROLE,
    definition_role, form_slot_role, form_variant_role, text_variant_role,
};

pub(crate) const MAX_ENTRY_NODES: usize = 2_000;

// --- forms ---

pub fn validate_forms(
    entry_id: Uuid,
    content: &DraftFormsStepContent,
    headwords: &WordHeadwordsV2,
    configured_parts: &HashSet<String>,
) -> Vec<DraftValidationIssue> {
    let mut issues = Vec::new();
    let mut node_types = HashMap::new();
    let mut pos_codes = HashSet::new();

    if content.pos.is_empty() {
        issue(
            &mut issues,
            PersistedWordStep::Forms,
            entry_id,
            "pos",
            "pos_required",
            "至少保留一个基本词性",
        );
    }
    for pos in &content.pos {
        unique_node(
            &mut issues,
            &mut node_types,
            PersistedWordStep::Forms,
            pos.pos_id,
            "pos",
        );
        if !pos_codes.insert(pos.pos.as_str()) {
            issue(
                &mut issues,
                PersistedWordStep::Forms,
                pos.pos_id,
                "pos",
                "duplicate_part_of_speech",
                "同一词条不能重复添加基本词性",
            );
        }
        if !configured_parts.contains(&pos.pos) {
            issue(
                &mut issues,
                PersistedWordStep::Forms,
                pos.pos_id,
                "pos",
                "unknown_part_of_speech",
                "基本词性未配置",
            );
        }
        if !matches!(
            pos.dialect_rules.spelling_mode.as_str(),
            "unified" | "distinguish"
        ) || !matches!(
            pos.dialect_rules.phonetic_mode.as_str(),
            "unified" | "distinguish"
        ) || (pos.dialect_rules.spelling_mode == "distinguish"
            && pos.dialect_rules.phonetic_mode != "distinguish")
        {
            issue(
                &mut issues,
                PersistedWordStep::Forms,
                pos.pos_id,
                "dialect_rules",
                "dialect_rules_invalid",
                "词形与音标方言规则无效",
            );
        }
        if pos.form_groups.is_empty() {
            issue(
                &mut issues,
                PersistedWordStep::Forms,
                pos.pos_id,
                "form_groups",
                "form_group_required",
                "每个词性至少需要一组词形变化",
            );
        }

        unique_node(
            &mut issues,
            &mut node_types,
            PersistedWordStep::Forms,
            pos.base_form.id,
            "form_slot",
        );
        if pos.base_form.form_type != "base" {
            issue(
                &mut issues,
                PersistedWordStep::Forms,
                pos.base_form.id,
                "form_type",
                "base_form_type_invalid",
                "共享原形的 form_type 必须为 base",
            );
        }
        validate_slot_variants(
            &mut issues,
            &mut node_types,
            pos.base_form.id,
            &pos.base_form.variants,
            &pos.dialect_rules.spelling_mode,
            Some(headwords),
        );

        for group in &pos.form_groups {
            unique_node(
                &mut issues,
                &mut node_types,
                PersistedWordStep::Forms,
                group.id,
                "form_group",
            );
            if group.slots.is_empty() {
                issue(
                    &mut issues,
                    PersistedWordStep::Forms,
                    group.id,
                    "slots",
                    "form_slot_required",
                    "每组词形变化至少需要一个词形",
                );
            }
            let mut form_types = HashSet::new();
            for slot in &group.slots {
                unique_node(
                    &mut issues,
                    &mut node_types,
                    PersistedWordStep::Forms,
                    slot.id,
                    "form_slot",
                );
                if slot.form_type == "base" || !valid_form_type(&slot.form_type) {
                    issue(
                        &mut issues,
                        PersistedWordStep::Forms,
                        slot.id,
                        "form_type",
                        "form_type_invalid",
                        "派生词形类型无效",
                    );
                }
                if !crate::lexicon::form_types::allowed_form_types(&pos.pos)
                    .contains(&slot.form_type.as_str())
                {
                    issue(
                        &mut issues,
                        PersistedWordStep::Forms,
                        slot.id,
                        "form_type",
                        "invalid_form_type_for_part_of_speech",
                        "词形类型不适用于当前基本词性",
                    );
                }
                if !form_types.insert(slot.form_type.as_str()) {
                    issue(
                        &mut issues,
                        PersistedWordStep::Forms,
                        group.id,
                        "slots",
                        "duplicate_form_type",
                        "同组内词形类型不能重复",
                    );
                }
                validate_slot_variants(
                    &mut issues,
                    &mut node_types,
                    slot.id,
                    &slot.variants,
                    &pos.dialect_rules.spelling_mode,
                    None,
                );
            }
        }
    }
    issues
}

// --- nodes ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProposedNode {
    pub id: Uuid,
    pub node_type: &'static str,
    pub step: PersistedWordStep,
    pub parent_node_id: Option<Uuid>,
    pub node_role: String,
    pub stable_slot: bool,
}

pub(crate) fn proposed_nodes(
    forms: &DraftFormsStepContent,
    meanings: &DraftMeaningsStepContent,
) -> Vec<ProposedNode> {
    let mut nodes = Vec::new();
    for pos in &forms.pos {
        push_node(
            &mut nodes,
            pos.pos_id,
            "pos",
            PersistedWordStep::Forms,
            None,
            POS_ROLE,
            false,
        );
        push_node(
            &mut nodes,
            pos.base_form.id,
            "form_slot",
            PersistedWordStep::Forms,
            Some(pos.pos_id),
            BASE_FORM_ROLE,
            true,
        );
        push_form_variant_nodes(&mut nodes, pos.base_form.id, &pos.base_form.variants);
        for group in &pos.form_groups {
            push_node(
                &mut nodes,
                group.id,
                "form_group",
                PersistedWordStep::Forms,
                Some(pos.pos_id),
                FORM_GROUP_ROLE,
                false,
            );
            for slot in &group.slots {
                push_node(
                    &mut nodes,
                    slot.id,
                    "form_slot",
                    PersistedWordStep::Forms,
                    Some(group.id),
                    form_slot_role(&slot.form_type),
                    true,
                );
                push_form_variant_nodes(&mut nodes, slot.id, &slot.variants);
            }
        }
    }
    for group in &meanings.sense_groups {
        push_node(
            &mut nodes,
            group.id,
            "sense_group",
            PersistedWordStep::Meanings,
            None,
            SENSE_GROUP_ROLE,
            false,
        );
    }
    for pos in &meanings.pos {
        for grammar in &pos.grammar_structures {
            push_node(
                &mut nodes,
                grammar.id,
                "grammar_structure",
                PersistedWordStep::Meanings,
                Some(pos.pos_id),
                GRAMMAR_STRUCTURE_ROLE,
                false,
            );
            for variant in &grammar.variants {
                push_node(
                    &mut nodes,
                    variant.id,
                    "text_variant",
                    PersistedWordStep::Meanings,
                    Some(grammar.id),
                    text_variant_role("content", "en", variant.dialect),
                    true,
                );
            }
        }
        for sense in &pos.senses {
            push_node(
                &mut nodes,
                sense.id,
                "sense",
                PersistedWordStep::Meanings,
                Some(pos.pos_id),
                SENSE_ROLE,
                false,
            );
            for definition in &sense.definitions {
                match definition {
                    WordDefinitionV2::ZhDefinition { id, content_id, .. }
                    | WordDefinitionV2::ZhSentence { id, content_id, .. } => {
                        push_node(
                            &mut nodes,
                            *id,
                            "definition",
                            PersistedWordStep::Meanings,
                            Some(sense.id),
                            definition_role(definition),
                            false,
                        );
                        push_node(
                            &mut nodes,
                            *content_id,
                            "text_variant",
                            PersistedWordStep::Meanings,
                            Some(*id),
                            text_variant_role("content", "zh", Dialect::Common),
                            true,
                        );
                    }
                    WordDefinitionV2::EnDefinition { id, content, .. }
                    | WordDefinitionV2::EnSentence { id, content, .. } => {
                        push_node(
                            &mut nodes,
                            *id,
                            "definition",
                            PersistedWordStep::Meanings,
                            Some(sense.id),
                            definition_role(definition),
                            false,
                        );
                        push_english_text_nodes(&mut nodes, *id, "content", content);
                    }
                }
            }
            for sentence in &sense.sentences {
                push_node(
                    &mut nodes,
                    sentence.id,
                    "sentence",
                    PersistedWordStep::Meanings,
                    Some(sense.id),
                    SENTENCE_ROLE,
                    false,
                );
                push_english_text_nodes(&mut nodes, sentence.id, "en_text", &sentence.en_text);
                push_node(
                    &mut nodes,
                    sentence.zh_text_id,
                    "text_variant",
                    PersistedWordStep::Meanings,
                    Some(sentence.id),
                    text_variant_role("zh_text", "zh", Dialect::Common),
                    true,
                );
            }
            for relation in &sense.relations {
                push_node(
                    &mut nodes,
                    relation.id,
                    "relation",
                    PersistedWordStep::Meanings,
                    Some(sense.id),
                    RELATION_ROLE,
                    false,
                );
            }
        }
    }
    nodes
}

pub(crate) fn validate_node_identities(
    entry_id: Uuid,
    proposed: &[ProposedNode],
    existing: &[NodeIdentityRecord],
) -> Vec<DraftValidationIssue> {
    let mut issues = Vec::new();
    let mut seen = HashMap::<Uuid, &ProposedNode>::new();
    for node in proposed {
        if let Some(previous) = seen.insert(node.id, node) {
            issue(
                &mut issues,
                node.step,
                node.id,
                "id",
                "node_id_reused",
                if previous.step == node.step {
                    "节点 ID 在请求中重复"
                } else {
                    "节点 ID 不能跨步骤复用"
                },
            );
        }
    }

    let proposed_by_id = proposed
        .iter()
        .map(|node| (node.id, node))
        .collect::<HashMap<_, _>>();
    for stored in existing {
        let Some(node) = proposed_by_id.get(&stored.id) else {
            continue;
        };
        if stored.entry_id != entry_id || stored.node_type != node.node_type {
            issue(
                &mut issues,
                node.step,
                node.id,
                "id",
                "node_id_reused",
                "节点 ID 已属于其他词条或节点类型",
            );
        } else if stored.node_role == LEGACY_NODE_ROLE {
            issue(
                &mut issues,
                node.step,
                node.id,
                "id",
                "node_binding_unknown",
                "历史节点缺少可验证的父子绑定，不能重新用于草稿内容",
            );
        } else if stored.parent_node_id != node.parent_node_id
            || stored.node_role != node.node_role
            || stored.stable_slot != node.stable_slot
        {
            issue(
                &mut issues,
                node.step,
                node.id,
                "id",
                "node_binding_changed",
                "节点 ID 不能更换父节点或内容槽位",
            );
        }
    }

    let existing_stable_slots = existing
        .iter()
        .filter(|node| node.entry_id == entry_id && node.stable_slot)
        .map(|node| ((node.parent_node_id, node.node_role.as_str()), node.id))
        .collect::<HashMap<_, _>>();
    for node in proposed.iter().filter(|node| node.stable_slot) {
        if existing_stable_slots
            .get(&(node.parent_node_id, node.node_role.as_str()))
            .is_some_and(|existing_id| *existing_id != node.id)
        {
            issue(
                &mut issues,
                node.step,
                node.id,
                "id",
                "stable_node_id_changed",
                "已有内容槽位必须保留原节点 ID",
            );
        }
    }
    issues
}

pub(crate) fn validate_node_limit(
    entry_id: Uuid,
    step: PersistedWordStep,
    proposed: &[ProposedNode],
) -> Vec<DraftValidationIssue> {
    if proposed.len() <= MAX_ENTRY_NODES {
        return Vec::new();
    }
    let mut issues = Vec::new();
    issue(
        &mut issues,
        step,
        entry_id,
        "content",
        "aggregate_node_limit_exceeded",
        &format!("单个词条最多包含 {MAX_ENTRY_NODES} 个内容节点"),
    );
    issues
}

fn push_node(
    nodes: &mut Vec<ProposedNode>,
    id: Uuid,
    node_type: &'static str,
    step: PersistedWordStep,
    parent_node_id: Option<Uuid>,
    node_role: impl Into<String>,
    stable_slot: bool,
) {
    nodes.push(ProposedNode {
        id,
        node_type,
        step,
        parent_node_id,
        node_role: node_role.into(),
        stable_slot,
    });
}

fn push_form_variant_nodes(
    nodes: &mut Vec<ProposedNode>,
    slot_id: Uuid,
    variants: &[crate::lexicon::dto::WordFormVariantV2],
) {
    for variant in variants {
        push_node(
            nodes,
            variant.id,
            "form_variant",
            PersistedWordStep::Forms,
            Some(slot_id),
            form_variant_role(variant.dialect),
            true,
        );
        for pronunciation in &variant.pronunciations {
            push_node(
                nodes,
                pronunciation.id,
                "pronunciation",
                PersistedWordStep::Forms,
                Some(variant.id),
                PRONUNCIATION_ROLE,
                false,
            );
        }
    }
}

fn push_english_text_nodes(
    nodes: &mut Vec<ProposedNode>,
    owner_id: Uuid,
    field_role: &str,
    value: &EnglishTextV2,
) {
    match value {
        EnglishTextV2::Unified { common } => push_node(
            nodes,
            common.id,
            "text_variant",
            PersistedWordStep::Meanings,
            Some(owner_id),
            text_variant_role(field_role, "en", Dialect::Common),
            true,
        ),
        EnglishTextV2::Distinguish { uk, us, .. } => {
            for (dialect, slot) in [(Dialect::Uk, uk), (Dialect::Us, us)] {
                if let DialectVariantSlotV2::Ready { variant } = slot {
                    push_node(
                        nodes,
                        variant.id,
                        "text_variant",
                        PersistedWordStep::Meanings,
                        Some(owner_id),
                        text_variant_role(field_role, "en", dialect),
                        true,
                    );
                }
            }
        }
    }
}

/// Rejects strings PostgreSQL cannot persist in either TEXT or JSONB.
///
/// Walking the serialized DTO keeps this guard exhaustive when a new textual
/// field is added to the forms or meanings wire shape.
pub(crate) fn validate_persisted_text<T: serde::Serialize>(
    entry_id: Uuid,
    step: PersistedWordStep,
    content: &T,
) -> Vec<DraftValidationIssue> {
    let contains_nul =
        serde_json::to_value(content).is_ok_and(|value| json_value_contains_nul(&value));
    if !contains_nul {
        return Vec::new();
    }

    let mut issues = Vec::new();
    issue(
        &mut issues,
        step,
        entry_id,
        "content",
        "nul_character_not_allowed",
        "文本字段不能包含 NUL 字符",
    );
    issues
}

fn json_value_contains_nul(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(value) => value.contains('\0'),
        serde_json::Value::Array(values) => values.iter().any(json_value_contains_nul),
        serde_json::Value::Object(values) => values
            .iter()
            .any(|(key, value)| key.contains('\0') || json_value_contains_nul(value)),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn forms(pos_and_types: &[(&str, &[&str])]) -> DraftFormsStepContent {
        let pos = pos_and_types
            .iter()
            .map(|(pos, form_types)| {
                let pos_id = Uuid::now_v7();
                serde_json::from_value(json!({
                    "pos_id": pos_id,
                    "pos": pos,
                    "dialect_rules": {"spelling_mode": "unified", "phonetic_mode": "unified"},
                    "base_form": {
                        "id": Uuid::now_v7(), "form_type": "base",
                        "variants": [{
                            "id": Uuid::now_v7(), "dialect": "common", "spelling": "high",
                            "origin": "dictionary", "pronunciations": [{
                                "id": Uuid::now_v7(), "dict_phonetic": "", "actual_pron": "", "style": "normal"
                            }]
                        }]
                    },
                    "form_groups": [{
                        "id": Uuid::now_v7(), "is_regular": true,
                        "slots": form_types.iter().map(|form_type| json!({
                            "id": Uuid::now_v7(), "form_type": form_type,
                            "variants": [{
                                "id": Uuid::now_v7(), "dialect": "common", "spelling": "value",
                                "origin": "manual", "pronunciations": [{
                                    "id": Uuid::now_v7(), "dict_phonetic": "", "actual_pron": "", "style": "normal"
                                }]
                            }]
                        })).collect::<Vec<_>>()
                    }]
                }))
                .unwrap()
            })
            .collect();
        DraftFormsStepContent { pos }
    }

    #[test]
    fn accepts_high_and_access_baseline_form_sets() {
        for content in [
            forms(&[
                ("noun", &["plural"]),
                ("adjective", &["comparative", "superlative"]),
            ]),
            forms(&[
                ("noun", &["plural"]),
                (
                    "verb",
                    &[
                        "third_person_singular",
                        "present_participle",
                        "past_tense",
                        "past_participle",
                    ],
                ),
            ]),
        ] {
            let configured = content.pos.iter().map(|pos| pos.pos.clone()).collect();
            let issues = validate_forms(
                Uuid::now_v7(),
                &content,
                &WordHeadwordsV2::Unified {
                    common: "high".to_owned(),
                },
                &configured,
            );
            assert!(
                issues
                    .iter()
                    .all(|issue| issue.code != "invalid_form_type_for_part_of_speech")
            );
        }
    }

    #[test]
    fn aggregates_every_pos_form_type_mismatch_at_the_slot() {
        let content = forms(&[
            ("adjective", &["past_tense"]),
            ("verb", &["comparative"]),
            ("noun", &["superlative"]),
        ]);
        let configured = content.pos.iter().map(|pos| pos.pos.clone()).collect();
        let issues = validate_forms(
            Uuid::now_v7(),
            &content,
            &WordHeadwordsV2::Unified {
                common: "high".to_owned(),
            },
            &configured,
        );
        let mismatches = issues
            .iter()
            .filter(|issue| issue.code == "invalid_form_type_for_part_of_speech")
            .collect::<Vec<_>>();
        assert_eq!(mismatches.len(), 3);
        assert_eq!(mismatches[0].step, PersistedWordStep::Forms);
        assert_eq!(mismatches[0].field, "form_type");
        assert_eq!(
            mismatches[0].node_id,
            content.pos[0].form_groups[0].slots[0].id
        );
        assert_eq!(
            mismatches[1].node_id,
            content.pos[1].form_groups[0].slots[0].id
        );
        assert_eq!(
            mismatches[2].node_id,
            content.pos[2].form_groups[0].slots[0].id
        );
    }
}
