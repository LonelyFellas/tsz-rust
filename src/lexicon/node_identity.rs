use crate::lexicon::dto::{Dialect, WordDefinitionV2};

pub(crate) const POS_ROLE: &str = "forms.pos";
pub(crate) const BASE_FORM_ROLE: &str = "forms.base_form";
pub(crate) const FORM_GROUP_ROLE: &str = "forms.form_group";
pub(crate) const PRONUNCIATION_ROLE: &str = "forms.pronunciation";
pub(crate) const SENSE_GROUP_ROLE: &str = "meanings.sense_group";
pub(crate) const GRAMMAR_STRUCTURE_ROLE: &str = "meanings.grammar_structure";
pub(crate) const SENSE_ROLE: &str = "meanings.sense";
pub(crate) const SENTENCE_ROLE: &str = "meanings.sentence";
pub(crate) const RELATION_ROLE: &str = "meanings.relation";
/// 释义级短语成分用词。变体级的 `forms.phrase_component_usage` B2 才退场，
/// B1 期间两个角色共用同一个 `node_type`。
pub(crate) const PHRASE_COMPONENT_USAGE_ROLE: &str = "meanings.phrase_component_usage";
pub(crate) const LEGACY_NODE_ROLE: &str = "legacy";

pub(crate) const FORM_SLOT_ROLE_PREFIX: &str = "forms.form_slot:";

pub(crate) fn form_slot_role(form_type: &str) -> String {
    format!("{FORM_SLOT_ROLE_PREFIX}{form_type}")
}

pub(crate) fn form_variant_role(dialect: Dialect) -> String {
    format!("forms.form_variant:{}", dialect_name(dialect))
}

pub(crate) fn definition_role(definition: &WordDefinitionV2) -> &'static str {
    match definition {
        WordDefinitionV2::ZhDefinition { .. } => "meanings.definition:zh:definition",
        WordDefinitionV2::ZhSentence { .. } => "meanings.definition:zh:sentence",
        WordDefinitionV2::EnDefinition { .. } => "meanings.definition:en:definition",
        WordDefinitionV2::EnSentence { .. } => "meanings.definition:en:sentence",
    }
}

pub(crate) fn text_variant_role(field_role: &str, language: &str, dialect: Dialect) -> String {
    format!("meanings.{field_role}:{language}:{}", dialect_name(dialect))
}

pub(crate) const fn dialect_name(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Common => "common",
        Dialect::Uk => "uk",
        Dialect::Us => "us",
    }
}

/// `dialect_name` 的逆向：节点角色末段还原成方言侧。角色不以方言结尾时返回 `None`。
pub(crate) fn dialect_from_name(name: &str) -> Option<Dialect> {
    match name {
        "common" => Some(Dialect::Common),
        "uk" => Some(Dialect::Uk),
        "us" => Some(Dialect::Us),
        _ => None,
    }
}
