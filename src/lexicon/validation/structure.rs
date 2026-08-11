use super::*;

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

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProposedNode {
    pub id: Uuid,
    pub node_type: &'static str,
    pub step: PersistedWordStep,
}

pub(crate) fn proposed_nodes(
    forms: &DraftFormsStepContent,
    meanings: &DraftMeaningsStepContent,
) -> Vec<ProposedNode> {
    let mut nodes = Vec::new();
    for pos in &forms.pos {
        push_node(&mut nodes, pos.pos_id, "pos", PersistedWordStep::Forms);
        push_node(
            &mut nodes,
            pos.base_form.id,
            "form_slot",
            PersistedWordStep::Forms,
        );
        push_form_variant_nodes(&mut nodes, &pos.base_form.variants);
        for group in &pos.form_groups {
            push_node(&mut nodes, group.id, "form_group", PersistedWordStep::Forms);
            for slot in &group.slots {
                push_node(&mut nodes, slot.id, "form_slot", PersistedWordStep::Forms);
                push_form_variant_nodes(&mut nodes, &slot.variants);
            }
        }
    }
    for group in &meanings.sense_groups {
        push_node(
            &mut nodes,
            group.id,
            "sense_group",
            PersistedWordStep::Meanings,
        );
    }
    for pos in &meanings.pos {
        for grammar in &pos.grammar_structures {
            push_node(
                &mut nodes,
                grammar.id,
                "grammar_structure",
                PersistedWordStep::Meanings,
            );
            for variant in &grammar.variants {
                push_node(
                    &mut nodes,
                    variant.id,
                    "text_variant",
                    PersistedWordStep::Meanings,
                );
            }
        }
        for sense in &pos.senses {
            push_node(&mut nodes, sense.id, "sense", PersistedWordStep::Meanings);
            for definition in &sense.definitions {
                match definition {
                    WordDefinitionV2::ZhDefinition { id, content_id, .. }
                    | WordDefinitionV2::ZhSentence { id, content_id, .. } => {
                        push_node(&mut nodes, *id, "definition", PersistedWordStep::Meanings);
                        push_node(
                            &mut nodes,
                            *content_id,
                            "text_variant",
                            PersistedWordStep::Meanings,
                        );
                    }
                    WordDefinitionV2::EnDefinition { id, content, .. }
                    | WordDefinitionV2::EnSentence { id, content, .. } => {
                        push_node(&mut nodes, *id, "definition", PersistedWordStep::Meanings);
                        push_english_text_nodes(&mut nodes, content);
                    }
                }
            }
            for sentence in &sense.sentences {
                push_node(
                    &mut nodes,
                    sentence.id,
                    "sentence",
                    PersistedWordStep::Meanings,
                );
                push_english_text_nodes(&mut nodes, &sentence.en_text);
                push_node(
                    &mut nodes,
                    sentence.zh_text_id,
                    "text_variant",
                    PersistedWordStep::Meanings,
                );
            }
            for relation in &sense.relations {
                push_node(
                    &mut nodes,
                    relation.id,
                    "relation",
                    PersistedWordStep::Meanings,
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
    let mut seen = HashMap::<Uuid, ProposedNode>::new();
    for node in proposed {
        if let Some(previous) = seen.insert(node.id, *node) {
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
        .map(|node| (node.id, *node))
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
        }
    }
    issues
}

fn push_node(
    nodes: &mut Vec<ProposedNode>,
    id: Uuid,
    node_type: &'static str,
    step: PersistedWordStep,
) {
    nodes.push(ProposedNode {
        id,
        node_type,
        step,
    });
}

fn push_form_variant_nodes(
    nodes: &mut Vec<ProposedNode>,
    variants: &[crate::lexicon::dto::WordFormVariantV2],
) {
    for variant in variants {
        push_node(nodes, variant.id, "form_variant", PersistedWordStep::Forms);
        for pronunciation in &variant.pronunciations {
            push_node(
                nodes,
                pronunciation.id,
                "pronunciation",
                PersistedWordStep::Forms,
            );
        }
    }
}

fn push_english_text_nodes(nodes: &mut Vec<ProposedNode>, value: &EnglishTextV2) {
    match value {
        EnglishTextV2::Unified { common } => push_node(
            nodes,
            common.id,
            "text_variant",
            PersistedWordStep::Meanings,
        ),
        EnglishTextV2::Distinguish { uk, us, .. } => {
            for slot in [uk, us] {
                if let DialectVariantSlotV2::Ready { variant } = slot {
                    push_node(
                        nodes,
                        variant.id,
                        "text_variant",
                        PersistedWordStep::Meanings,
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
